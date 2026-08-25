use std::{
    sync::{Arc, Mutex, atomic::AtomicU64},
    time::Duration,
};

use agent_contracts::{HeartbeatRun, WorkerDirective};
use chrono::{DateTime, Utc};
use execution_runtime::OperationContext;
use tokio::task::JoinHandle;
use uuid::Uuid;

use crate::clients::AgentClient;

use super::{RunInterruption, service::LeaseCredentials};

const RETRY_DELAY: Duration = Duration::from_millis(250);
const MAX_REQUEST_TIME: Duration = Duration::from_secs(5);

pub(super) struct HeartbeatState {
    pub state_version: Arc<AtomicU64>,
    pub expires_at: Arc<Mutex<DateTime<Utc>>>,
    pub interruption: Arc<Mutex<Option<RunInterruption>>>,
    pub run_operation: OperationContext,
}

pub(super) fn spawn(
    agent: AgentClient,
    run_id: Uuid,
    lease: Arc<LeaseCredentials>,
    state: HeartbeatState,
    stop: OperationContext,
    shutdown: OperationContext,
) -> JoinHandle<()> {
    tokio::spawn(async move {
        loop {
            let expires_at = *state.expires_at.lock().expect("heartbeat expiry lock");
            let Some(remaining) = time_remaining(expires_at) else {
                interrupt(&state, RunInterruption::LeaseLost);
                return;
            };
            let delay = remaining / 3;
            tokio::select! {
                () = stop.cancelled() => return,
                () = shutdown.cancelled() => return,
                () = tokio::time::sleep(delay) => {}
            }

            loop {
                let expires_at = *state.expires_at.lock().expect("heartbeat expiry lock");
                let Some(remaining) = time_remaining(expires_at) else {
                    interrupt(&state, RunInterruption::LeaseLost);
                    return;
                };
                let request_time = MAX_REQUEST_TIME.min(remaining / 2);
                let command = HeartbeatRun {
                    lease_version: lease.version(),
                };
                let heartbeat = agent.heartbeat(run_id, &command, lease.token());
                let result = tokio::select! {
                    () = stop.cancelled() => return,
                    () = shutdown.cancelled() => return,
                    result = tokio::time::timeout(request_time, heartbeat) => result,
                };
                match result {
                    Ok(Ok(heartbeat)) => {
                        if heartbeat.lease_id != lease.lease_id
                            || heartbeat.lease_version != lease.version()
                            || heartbeat.expires_at <= Utc::now()
                        {
                            interrupt(&state, RunInterruption::LeaseLost);
                            return;
                        }
                        state.state_version.store(
                            heartbeat.state_version,
                            std::sync::atomic::Ordering::Release,
                        );
                        *state.expires_at.lock().expect("heartbeat expiry lock") =
                            heartbeat.expires_at;
                        for directive in heartbeat.directives {
                            let WorkerDirective::Abort(abort) = directive;
                            interrupt(&state, RunInterruption::Abort(abort));
                        }
                        break;
                    }
                    Ok(Err(error)) if error.retryable() => {
                        if !retry(&stop, &shutdown).await {
                            return;
                        }
                    }
                    Err(_) => {
                        if !retry(&stop, &shutdown).await {
                            return;
                        }
                    }
                    Ok(Err(_)) => {
                        interrupt(&state, RunInterruption::LeaseLost);
                        return;
                    }
                }
            }
        }
    })
}

fn time_remaining(expires_at: DateTime<Utc>) -> Option<Duration> {
    (expires_at - Utc::now())
        .to_std()
        .ok()
        .filter(|duration| !duration.is_zero())
}

async fn retry(stop: &OperationContext, shutdown: &OperationContext) -> bool {
    tokio::select! {
        () = stop.cancelled() => false,
        () = shutdown.cancelled() => false,
        () = tokio::time::sleep(RETRY_DELAY) => true,
    }
}

fn interrupt(state: &HeartbeatState, interruption: RunInterruption) {
    let mut current = state.interruption.lock().expect("run interruption lock");
    if current.is_none() {
        *current = Some(interruption);
    }
    state.run_operation.cancel();
}
