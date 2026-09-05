use crate::Registry;
use futures_util::FutureExt;
use harness_runtime::{Execution, Signals};
use platform_runtime_client::{
    Command, Error, PlatformClient, RequestKey, Result, WorkerRegistration, types::*,
};
use serde::Serialize;
use std::{
    collections::{HashMap, HashSet},
    panic::AssertUnwindSafe,
    sync::Arc,
    time::Duration,
};
use tokio::{
    sync::{RwLock, watch},
    task::{AbortHandle, JoinSet},
    time::{Instant, MissedTickBehavior},
};
use uuid::Uuid;

#[derive(Clone)]
pub struct Settings {
    pub build_id: String,
    pub capacity: u32,
    pub heartbeat_interval: Duration,
    pub poll_interval: Duration,
    pub input_interval: Duration,
    pub shutdown_grace: Duration,
}
impl Default for Settings {
    fn default() -> Self {
        Self {
            build_id: env!("CARGO_PKG_VERSION").into(),
            capacity: 8,
            heartbeat_interval: Duration::from_secs(15),
            poll_interval: Duration::from_secs(2),
            input_interval: Duration::from_secs(1),
            shutdown_grace: Duration::from_secs(30),
        }
    }
}
#[derive(Clone, Serialize)]
pub struct Snapshot {
    pub worker_id: Uuid,
    pub registered: bool,
    pub draining: bool,
    pub active_runs: usize,
    pub supported_harnesses: Vec<String>,
    pub heartbeat_ok: bool,
    pub activations: u64,
    pub lost_leases: u64,
}
pub struct Supervisor {
    client: PlatformClient,
    registry: Registry,
    settings: Settings,
    snapshot: Arc<RwLock<Snapshot>>,
}
struct Active {
    lease: Lease,
    deadline: Instant,
    signals: watch::Sender<Signals>,
    abort: AbortHandle,
}
impl Drop for Active {
    fn drop(&mut self) {
        self.abort.abort();
    }
}
enum Network {
    Drain(Result<()>),
    Heartbeat(Instant, Vec<Lease>, Result<HeartbeatResponse>),
    Allocation(Instant, Result<(AssignmentsResponse, HeartbeatResponse)>),
}
fn key(lease: Lease) -> (Uuid, i64) {
    (lease.run_id, lease.lease_epoch)
}
fn deadline(start: Instant, seconds: u32) -> Instant {
    // Based on request start, not wall-clock comparison with the server.
    start + Duration::from_secs(u64::from(seconds.saturating_sub(5)))
}
fn heartbeat_error(error: Error) -> Error {
    // Heartbeat and work-availability interfaces guarantee this means offline.
    // A claim can also conflict during an ordinary administrative drain.
    if error.conflict() == Some(ConflictCode::WorkerStateConflict) {
        Error::WorkerOffline
    } else {
        error
    }
}
impl Supervisor {
    pub fn new(client: PlatformClient, registry: Registry, settings: Settings) -> Result<Self> {
        if !(1..=200).contains(&settings.capacity)
            || settings.heartbeat_interval.is_zero()
            || settings.heartbeat_interval > Duration::from_secs(20)
            || settings.poll_interval.is_zero()
            || settings.input_interval.is_zero()
            || settings.shutdown_grace.is_zero()
        {
            return Err(Error::Invalid(
                "invalid worker capacity or supervision intervals",
            ));
        }
        let snapshot = Arc::new(RwLock::new(Snapshot {
            worker_id: client.worker_id(),
            registered: false,
            draining: false,
            active_runs: 0,
            supported_harnesses: registry.ids(),
            heartbeat_ok: false,
            activations: 0,
            lost_leases: 0,
        }));
        Ok(Self {
            client,
            registry,
            settings,
            snapshot,
        })
    }
    pub fn snapshot(&self) -> Arc<RwLock<Snapshot>> {
        self.snapshot.clone()
    }

    /// The bootstrap is discarded after registration. Closing `stop` also drains.
    pub async fn run(
        self,
        bootstrap: zeroize::Zeroizing<String>,
        mut stop: watch::Receiver<bool>,
    ) -> Result<()> {
        self.client
            .register(
                &bootstrap,
                &WorkerRegistration {
                    build_id: self.settings.build_id.clone(),
                    capacity: self.settings.capacity as i32,
                    supported_harnesses: self.registry.ids(),
                },
            )
            .await?;
        drop(bootstrap);
        self.snapshot.write().await.registered = true;
        tracing::info!(worker_id=%self.client.worker_id(), "worker registered");
        let mut active: HashMap<Uuid, Active> = HashMap::new();
        let mut retired = HashSet::new();
        let mut network = JoinSet::new();
        // A separate, cancellable long poll cannot block heartbeat/allocation IO.
        let mut work_wait = JoinSet::new();
        let mut next_work_wait = Instant::now();
        let mut work_wait_backoff = self.settings.poll_interval;
        let mut executions = JoinSet::new();
        let mut heartbeat_busy = false;
        let mut allocation_busy = false;
        let mut pending_claim = None;
        let mut heartbeat = tokio::time::interval(self.settings.heartbeat_interval);
        heartbeat.set_missed_tick_behavior(MissedTickBehavior::Skip);
        let mut tick = tokio::time::interval(Duration::from_millis(100));
        tick.set_missed_tick_behavior(MissedTickBehavior::Skip);
        let mut next_poll = Instant::now();
        let mut backoff = self.settings.poll_interval;
        let mut draining = false;
        let mut drain_busy = false;
        let mut shutdown_at = None;
        let failure = loop {
            tokio::select! {
                _ = stop.changed(), if !draining => {
                    if *stop.borrow() || stop.has_changed().is_err() { draining = true; }
                }
                Some(result) = work_wait.join_next() => {
                    match result {
                        Ok(Ok(WorkAvailability { available })) => {
                            work_wait_backoff = self.settings.poll_interval;
                            if available { next_poll = Instant::now(); }
                            // Bound repeated hints when several workers race to claim.
                            next_work_wait = Instant::now() + Duration::from_millis(100);
                        }
                        Ok(Err(e)) => {
                            let error = heartbeat_error(e);
                            if matches!(error, Error::WorkerOffline) { break Some(error); }
                            tracing::warn!(error=%error, "work notification unavailable; allocation polling remains active");
                            next_work_wait = Instant::now() + work_wait_backoff
                                + Duration::from_millis(rand::random::<u8>() as u64);
                            work_wait_backoff = (work_wait_backoff * 2).min(Duration::from_secs(15));
                        }
                        Err(e) if e.is_cancelled() => {}
                        Err(_) => break Some(Error::Protocol("work notification task panicked")),
                    }
                }
                _ = heartbeat.tick(), if !heartbeat_busy => {
                    heartbeat_busy = true;
                    let client = self.client.clone();
                    let leases: Vec<_> = active.values().map(|a| a.lease).collect();
                    network.spawn(async move {
                        let start = Instant::now();
                        let response = client.heartbeat(&Heartbeat { leases: leases.clone() }).await.map_err(heartbeat_error);
                        Network::Heartbeat(start, leases, response)
                    });
                }
                Some(result) = network.join_next() => {
                    let event = result.map_err(|_| Error::Protocol("supervision task panicked"))?;
                    match event {
                        Network::Drain(result) => {
                            drain_busy = false;
                            if let Err(e) = result { tracing::warn!(error=%e, "drain notification failed; stopping local claims regardless"); }
                        }
                        Network::Heartbeat(start, requested, result) => {
                            heartbeat_busy = false;
                            self.snapshot.write().await.heartbeat_ok = result.is_ok();
                            match result {
                                Ok(reply) => {
                                    let until = deadline(start, reply.lease_duration_seconds);
                                    for lease in requested {
                                        if let Some(a) = active.get_mut(&lease.run_id).filter(|a| key(a.lease) == key(lease)) {
                                            if let Some(renewed) = reply.renewed.iter().find(|r| (r.run_id,r.lease_epoch)==key(lease)) {
                                                a.deadline = a.deadline.max(until);
                                                if renewed.abort_requested_at.is_some() { a.signals.send_modify(|s| s.abort_requested = true); }
                                            } else { a.deadline = Instant::now(); }
                                        }
                                    }
                                }
                                Err(e @ Error::WorkerOffline) => break Some(e),
                                Err(e) => tracing::warn!(error=%e, "heartbeat failed; local lease deadlines remain in force"),
                            }
                        }
                        Network::Allocation(start, result) => {
                            allocation_busy = false;
                            match result {
                                Ok((assignments, renewed)) => {
                                    pending_claim = None;
                                    if !matches!(assignments.worker.status, WorkerStatus::Accepting) { draining = true; }
                                    let current: HashSet<_> = assignments.items.iter().map(|a| key(a.lease())).collect();
                                    retired.retain(|lease| current.contains(lease));
                                    let until = deadline(start, renewed.lease_duration_seconds);
                                    let mut started = false;
                                    for assignment in assignments.items {
                                        let lease = assignment.lease();
                                        if draining || active.contains_key(&lease.run_id) || retired.contains(&key(lease))
                                            || active.len() >= self.settings.capacity as usize || until <= Instant::now() { continue; }
                                        let Some(renewed) = renewed.renewed.iter().find(|r| (r.run_id,r.lease_epoch)==key(lease)) else { continue; };
                                        let Some(harness) = self.registry.0.get(&assignment.harness_id).cloned() else {
                                            tracing::error!(run_id=%lease.run_id, "assignment has no compiled harness; draining");
                                            draining = true; break;
                                        };
                                        let (signals, receiver) = watch::channel(Signals { abort_requested: renewed.abort_requested_at.is_some(), ..Default::default() });
                                        let client = self.client.run(lease)?;
                                        let input_client = client.clone();
                                        let input_signals = signals.clone();
                                        let interval = self.settings.input_interval;
                                        let task = tokio::spawn(async move {
                                            tokio::select! {
                                                outcome = AssertUnwindSafe(harness.run(Execution { client, signals: receiver })).catch_unwind() => {
                                                    match outcome {
                                                        Ok(Ok(())) => {},
                                                        Ok(Err(e)) => tracing::warn!(error=%e, "harness activation failed; recovery remains harness-owned"),
                                                        Err(_) => tracing::error!("harness panicked; lease will expire for recovery"),
                                                    }
                                                }
                                                _ = poll_inputs(input_client, input_signals, interval) => {}
                                            }
                                        });
                                        let abort = task.abort_handle();
                                        executions.spawn(async move { let _ = task.await; lease });
                                        active.insert(lease.run_id, Active { lease, deadline: until, signals, abort });
                                        self.snapshot.write().await.activations += 1;
                                        started = true;
                                    }
                                    backoff = if started { self.settings.poll_interval } else { (backoff * 2).min(Duration::from_secs(15)) };
                                }
                                Err(e @ Error::WorkerOffline) => break Some(e),
                                Err(e) => {
                                    tracing::warn!(error=%e, "assignment reconciliation failed; retaining claim identity");
                                    backoff = (backoff * 2).min(Duration::from_secs(15));
                                }
                            }
                            next_poll = Instant::now() + backoff + Duration::from_millis(rand::random::<u8>() as u64);
                        }
                    }
                }
                Some(result) = executions.join_next() => {
                    if let Ok(lease) = result {
                        if active.get(&lease.run_id).is_some_and(|a| key(a.lease)==key(lease)) {
                            if let Some(a) = active.remove(&lease.run_id) {
                                if a.signals.borrow().ownership_lost { self.snapshot.write().await.lost_leases += 1; }
                            }
                        }
                        // Never restart a returned/panicked activation under the same
                        // epoch. If it failed to yield, stop renewing and let it expire.
                        retired.insert(key(lease));
                        next_poll = Instant::now();
                    }
                }
                _ = tick.tick() => {}
            }
            let expired: Vec<_> = active
                .values()
                .filter(|a| a.deadline <= Instant::now())
                .map(|a| a.lease)
                .collect();
            for lease in expired {
                if let Some(a) = active.remove(&lease.run_id) {
                    a.signals.send_modify(|s| s.ownership_lost = true);
                    retired.insert(key(lease));
                    self.snapshot.write().await.lost_leases += 1;
                }
            }
            if *stop.borrow() {
                draining = true;
            }
            if draining && shutdown_at.is_none() {
                shutdown_at = Some(Instant::now() + self.settings.shutdown_grace);
                self.snapshot.write().await.draining = true;
                for a in active.values() {
                    a.signals.send_modify(|s| s.draining = true);
                }
                // Do not await network IO in the supervisor: heartbeats/deadlines
                // must progress during a slow lifecycle request too.
                let client = self.client.clone();
                network.spawn(async move {
                    Network::Drain(
                        client
                            .patch_worker(&PatchWorker {
                                status: Some(WorkerStatus::Draining),
                                capacity: None,
                            })
                            .await
                            .map(|_| ()),
                    )
                });
                drain_busy = true;
            }
            self.snapshot.write().await.active_runs = active.len();
            if draining
                && (active.is_empty() && !allocation_busy && !drain_busy
                    || shutdown_at.is_some_and(|at| Instant::now() >= at))
            {
                break None;
            }
            if !draining && !allocation_busy && Instant::now() >= next_poll {
                work_wait.abort_all();
                allocation_busy = true;
                let command = pending_claim
                    .get_or_insert_with(|| {
                        Arc::new(Command::new(
                            RequestKey::new(Uuid::new_v4().to_string())
                                .expect("UUID is a valid request key"),
                            Claim {
                                limit: self
                                    .settings
                                    .capacity
                                    .saturating_sub(active.len() as u32)
                                    .max(1),
                            },
                        ))
                    })
                    .clone();
                let client = self.client.clone();
                let excluded = retired.clone();
                let can_claim =
                    !self.registry.0.is_empty() && active.len() < self.settings.capacity as usize;
                network.spawn(async move {
                    let start = Instant::now();
                    let result = async {
                        let mut assignments = client.assignments().await?;
                        if matches!(assignments.worker.status, WorkerStatus::Offline) {
                            return Err(Error::WorkerOffline);
                        }
                        if can_claim && matches!(assignments.worker.status, WorkerStatus::Accepting)
                        {
                            client.claim(&command).await?;
                            assignments = client.assignments().await?;
                        }
                        let leases = assignments
                            .items
                            .iter()
                            .map(|a| a.lease())
                            .filter(|l| !excluded.contains(&key(*l)))
                            .collect();
                        let heartbeat = client
                            .heartbeat(&Heartbeat { leases })
                            .await
                            .map_err(heartbeat_error)?;
                        Ok((assignments, heartbeat))
                    }
                    .await;
                    Network::Allocation(start, result)
                });
            }
            let spare_capacity =
                !self.registry.0.is_empty() && active.len() < self.settings.capacity as usize;
            if draining || !spare_capacity {
                work_wait.abort_all();
            } else if !allocation_busy
                && pending_claim.is_none()
                && work_wait.is_empty()
                && Instant::now() >= next_work_wait
            {
                let client = self.client.clone();
                work_wait.spawn(async move { client.wait_for_work(MAX_WORK_WAIT_SECONDS).await });
            }
        };
        if failure.is_some() {
            for a in active.values() {
                a.signals.send_modify(|s| s.ownership_lost = true);
            }
            let mut snapshot = self.snapshot.write().await;
            snapshot.draining = true;
            snapshot.registered = false;
            snapshot.heartbeat_ok = false;
            snapshot.lost_leases += active.len() as u64;
        }
        // Cancels local futures only. Never writes aborted/completed, never kills
        // children or external gateway operations. Unreleased leases expire.
        active.clear();
        work_wait.abort_all();
        while work_wait.join_next().await.is_some() {}
        network.abort_all();
        while network.join_next().await.is_some() {}
        while executions.join_next().await.is_some() {}
        self.snapshot.write().await.active_runs = 0;
        if let Some(error) = failure {
            // The bootstrap was discarded. Do not attempt to resurrect this
            // identity or wait on another lifecycle request. main returns a
            // nonzero exit status; the process supervisor can start a fresh one.
            return Err(error);
        }
        match self
            .client
            .patch_worker(&PatchWorker {
                status: Some(WorkerStatus::Offline),
                capacity: None,
            })
            .await
        {
            Ok(_) => tracing::info!("worker offline"),
            Err(e) => {
                tracing::warn!(error=%e, "worker stopped; Platform will expire remaining ownership")
            }
        }
        self.snapshot.write().await.registered = false;
        Ok(())
    }
}

async fn poll_inputs(
    client: platform_runtime_client::RunClient,
    signals: watch::Sender<Signals>,
    interval: Duration,
) {
    loop {
        match client
            .inputs(&SequenceQuery {
                limit: Some(1),
                ..Default::default()
            })
            .await
        {
            Ok(page) if !page.items.is_empty() => {
                signals.send_modify(|s| s.input_generation = s.input_generation.wrapping_add(1))
            }
            Err(e) if e.conflict() == Some(ConflictCode::LeaseLost) => {
                signals.send_modify(|s| s.ownership_lost = true);
                return;
            }
            Err(e) => tracing::warn!(error=%e, "input polling failed"),
            _ => {}
        }
        tokio::time::sleep(interval).await;
    }
}
