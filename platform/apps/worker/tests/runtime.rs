//! Real Platform HTTP + PostgreSQL. Harness fixtures are never linked by main.

fn gated_registry(gate: Arc<tokio::sync::Semaphore>) -> Registry {
    registry(move |execution| {
        let gate = gate.clone();
        Box::pin(async move {
            gate.acquire().await.unwrap().forget();
            let ctx = execution.client.context(&ContextQuery::default()).await?;
            let mut commit = Commit::new(ctx.run.run.version, ctx.session.current_revision);
            let inputs = execution.client.inputs(&SequenceQuery::default()).await?;
            commit.input_results = inputs
                .items
                .into_iter()
                .map(|input| InputResult {
                    id: input.id,
                    status: InputStatus::Handled,
                    handling: Default::default(),
                })
                .collect();
            let final_id = Uuid::now_v7();
            commit.messages.push(AppendMessage {
                message_id: final_id,
                message: serde_json::from_value(json!({"role":"assistant","id":"final","timestamp":0,"model":{"provider":"fixture","id":"test"},"duration_ms":1,"native_message":{},"content":[{"type":"response","response":{"content":"Done"}}],"stop_reason":"stop"})).unwrap(),
            });
            commit.disposition = Disposition::Completed {
                final_message_id: final_id,
            };
            execution.client.commit(&command(commit)).await?;
            Ok(())
        })
    })
}

#[sqlx::test(migrations = "../server/migrations")]
#[ignore = "requires DATABASE_URL; isolated SQLx database"]
async fn notifications_pick_up_work_with_spare_capacity_without_waiting_for_poll(pool: PgPool) {
    let app = App::new(pool).await;
    let gate = Arc::new(tokio::sync::Semaphore::new(0));
    let worker = app.worker_with_settings(
        gated_registry(gate.clone()),
        Settings {
            capacity: 2,
            poll_interval: Duration::from_secs(10),
            heartbeat_interval: Duration::from_millis(100),
            ..Default::default()
        },
    );
    until(async || app.faults.work_waits.load(Ordering::SeqCst) >= 1).await;
    // Prove idle long polls do not block heartbeat or spin on claims.
    let heartbeats = app.faults.heartbeats.load(Ordering::SeqCst);
    let claims = app.faults.claims.load(Ordering::SeqCst);
    tokio::time::sleep(Duration::from_millis(350)).await;
    assert!(app.faults.heartbeats.load(Ordering::SeqCst) > heartbeats);
    assert_eq!(app.faults.claims.load(Ordering::SeqCst), claims);
    let start = std::time::Instant::now();
    let first = app.create().await;
    until(async || worker.snapshot.read().await.active_runs == 1).await;
    assert!(start.elapsed() < Duration::from_secs(2));
    until(async || app.faults.work_waits.load(Ordering::SeqCst) >= 2).await;
    let start = std::time::Instant::now();
    let second = app.create().await;
    until(async || worker.snapshot.read().await.active_runs == 2).await;
    assert!(
        start.elapsed() < Duration::from_secs(2),
        "a busy worker must fill its free slot"
    );
    tokio::time::sleep(Duration::from_millis(150)).await;
    let waits = app.faults.work_waits.load(Ordering::SeqCst);
    let third = app.create().await;
    tokio::time::sleep(Duration::from_millis(300)).await;
    assert_eq!(app.status(third).await, "ready");
    assert_eq!(
        app.faults.work_waits.load(Ordering::SeqCst),
        waits,
        "no new waits at full capacity"
    );
    gate.add_permits(1);
    until(async || app.status(third).await == "running").await;
    gate.add_permits(2);
    until(async || {
        app.status(first).await == "completed"
            && app.status(second).await == "completed"
            && app.status(third).await == "completed"
    })
    .await;
    until(async || worker.snapshot.read().await.active_runs == 0).await;
    until(async || app.faults.work_waits.load(Ordering::SeqCst) > waits).await;
    let start = std::time::Instant::now();
    worker.shutdown().await;
    assert!(
        start.elapsed() < Duration::from_secs(2),
        "shutdown must cancel the long poll"
    );
}

#[sqlx::test(migrations = "../server/migrations")]
#[ignore = "requires DATABASE_URL; isolated SQLx database"]
async fn notification_failures_preserve_polling_and_reconnect(pool: PgPool) {
    let app = App::new(pool).await;
    app.faults.fail_work_wait.store(true, Ordering::SeqCst);
    let gate = Arc::new(tokio::sync::Semaphore::new(0));
    let worker = app.worker(gated_registry(gate.clone()));
    until(async || app.faults.work_waits.load(Ordering::SeqCst) > 0).await;
    let first = app.create().await;
    until(async || app.status(first).await == "running").await;
    gate.add_permits(1);
    until(async || worker.snapshot.read().await.active_runs == 0).await;
    app.faults.fail_work_wait.store(false, Ordering::SeqCst);
    let waits = app.faults.work_waits.load(Ordering::SeqCst);
    until(async || app.faults.work_waits.load(Ordering::SeqCst) > waits).await;
    let second = app.create().await;
    until(async || app.status(second).await == "running").await;
    gate.add_permits(1);
    until(async || worker.snapshot.read().await.active_runs == 0).await;
    worker.shutdown().await;
}
use axum::{
    extract::{Request, State},
    http::StatusCode,
    middleware::{self, Next},
    response::{IntoResponse, Response},
};
use futures_util::future::BoxFuture;
use harness_runtime::{Execution, Harness};
use platform_runtime_client::{
    ClientConfig, Command, PlatformClient, RequestKey, Result, types::*,
};
use platform_server::runtime::{RuntimeService, router, worker_router};
use platform_worker::{Registry, Settings, Snapshot, Supervisor};
use serde_json::{Value, json};
use sqlx::PgPool;
use std::{
    sync::{
        Arc,
        atomic::{AtomicBool, AtomicUsize, Ordering},
    },
    time::Duration,
};
use tokio::sync::{RwLock, watch};
use uuid::Uuid;

const BOOT: &str = "worker-integration-bootstrap-01234567890123456789";
#[derive(Default)]
struct Faults {
    fail_work_wait: AtomicBool,
    work_waits: AtomicUsize,
    heartbeats: AtomicUsize,
    claims: AtomicUsize,
    lose_claim: AtomicBool,
    fail_heartbeat: AtomicBool,
    fail_assignments: AtomicBool,
    disconnected: AtomicBool,
    short_lease: AtomicBool,
}
struct App {
    pool: PgPool,
    url: String,
    project: Uuid,
    task: tokio::task::JoinHandle<()>,
    reconciler: tokio::task::JoinHandle<()>,
    faults: Arc<Faults>,
}
impl Drop for App {
    fn drop(&mut self) {
        self.task.abort();
        self.reconciler.abort();
    }
}
async fn lose_claim_reply(
    State(faults): State<Arc<Faults>>,
    request: Request,
    next: Next,
) -> Response {
    let claim = request.uri().path().ends_with("/claims");
    let heartbeat = request.uri().path().ends_with("/heartbeat");
    let assignments = request.uri().path().ends_with("/assignments");
    let work_wait = request.uri().path().ends_with("/work-available");
    if claim {
        faults.claims.fetch_add(1, Ordering::SeqCst);
    }
    if heartbeat {
        faults.heartbeats.fetch_add(1, Ordering::SeqCst);
    }
    if work_wait {
        faults.work_waits.fetch_add(1, Ordering::SeqCst);
        if faults.fail_work_wait.load(Ordering::SeqCst) {
            return StatusCode::SERVICE_UNAVAILABLE.into_response();
        }
    }
    if faults.disconnected.load(Ordering::SeqCst)
        || assignments && faults.fail_assignments.load(Ordering::SeqCst)
    {
        return StatusCode::SERVICE_UNAVAILABLE.into_response();
    }
    if heartbeat && faults.fail_heartbeat.load(Ordering::SeqCst) {
        return StatusCode::SERVICE_UNAVAILABLE.into_response();
    }
    let response = next.run(request).await;
    if claim && faults.lose_claim.swap(false, Ordering::SeqCst) {
        StatusCode::SERVICE_UNAVAILABLE.into_response()
    } else if heartbeat
        && response.status().is_success()
        && faults.short_lease.load(Ordering::SeqCst)
    {
        let bytes = axum::body::to_bytes(response.into_body(), 1024 * 1024)
            .await
            .unwrap();
        let mut value: Value = serde_json::from_slice(&bytes).unwrap();
        value["lease_duration_seconds"] = json!(6);
        axum::Json(value).into_response()
    } else {
        response
    }
}
impl App {
    async fn new(pool: PgPool) -> Self {
        let project = Uuid::now_v7();
        sqlx::query("insert into projects(project_id,name) values($1,'Worker architecture test')")
            .bind(project)
            .execute(&pool)
            .await
            .unwrap();
        sqlx::query("insert into harnesses(id,name) values('fixture','Test-only fixture')")
            .execute(&pool)
            .await
            .unwrap();
        sqlx::query("insert into project_harnesses(project_id,harness_id,enabled) values($1,'fixture',true)")
            .bind(project).execute(&pool).await.unwrap();
        let runtime = RuntimeService::new(pool.clone());
        let reconciler = runtime.spawn_reconciler();
        let faults = Arc::new(Faults::default());
        let app = router(runtime.clone())
            .merge(worker_router(runtime, BOOT))
            .layer(middleware::from_fn_with_state(
                faults.clone(),
                lose_claim_reply,
            ));
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("http://{}", listener.local_addr().unwrap());
        let task = tokio::spawn(async move {
            axum::serve(listener, app).await.unwrap();
        });
        Self {
            pool,
            url,
            project,
            task,
            reconciler,
            faults,
        }
    }
    async fn post(&self, path: &str, body: Value) -> Value {
        reqwest::Client::new()
            .post(format!("{}{path}", self.url))
            .header("Idempotency-Key", Uuid::new_v4().to_string())
            .json(&body)
            .send()
            .await
            .unwrap()
            .error_for_status()
            .unwrap()
            .json()
            .await
            .unwrap()
    }
    async fn create(&self) -> Uuid {
        let result = self.post(&format!("/api/projects/{}/sessions", self.project), json!({"harness_id":"fixture", "initial_run":{"expected_session_revision":0,"input":{"role":"user","id":"task","timestamp":0,"content":[{"type":"text","content":"Work"}]}}})).await;
        serde_json::from_value(result["run"]["id"].clone()).unwrap()
    }
    fn worker(&self, registry: Registry) -> Running {
        self.worker_with_settings(
            registry,
            Settings {
                capacity: 1,
                heartbeat_interval: Duration::from_millis(100),
                poll_interval: Duration::from_millis(30),
                input_interval: Duration::from_millis(30),
                shutdown_grace: Duration::from_secs(2),
                ..Default::default()
            },
        )
    }
    fn worker_with_settings(&self, registry: Registry, settings: Settings) -> Running {
        let client = PlatformClient::new(
            &self.url,
            Uuid::new_v4(),
            format!("{}{}", Uuid::new_v4(), Uuid::new_v4()),
            ClientConfig {
                max_attempts: 1,
                ..Default::default()
            },
        )
        .unwrap();
        let worker = Supervisor::new(client, registry, settings).unwrap();
        let snapshot = worker.snapshot();
        let (stop, stopped) = watch::channel(false);
        let task = tokio::spawn(worker.run(zeroize::Zeroizing::new(BOOT.into()), stopped));
        Running {
            stop,
            task,
            snapshot,
        }
    }
    async fn status(&self, run: Uuid) -> String {
        sqlx::query_scalar("select status from runs where id=$1")
            .bind(run)
            .fetch_one(&self.pool)
            .await
            .unwrap()
    }
}
struct Running {
    stop: watch::Sender<bool>,
    task: tokio::task::JoinHandle<Result<()>>,
    snapshot: Arc<RwLock<Snapshot>>,
}
impl Running {
    async fn shutdown(self) {
        self.stop.send(true).unwrap();
        tokio::time::timeout(Duration::from_secs(5), self.task)
            .await
            .unwrap()
            .unwrap()
            .unwrap();
    }
}
struct Fixture<F>(F);
impl<F> Harness for Fixture<F>
where
    F: Fn(Execution) -> BoxFuture<'static, Result<()>> + Send + Sync,
{
    fn run(&self, execution: Execution) -> BoxFuture<'static, Result<()>> {
        (self.0)(execution)
    }
}
fn registry(
    f: impl Fn(Execution) -> BoxFuture<'static, Result<()>> + Send + Sync + 'static,
) -> Registry {
    let mut registry = Registry::default();
    registry.register("fixture", Arc::new(Fixture(f))).unwrap();
    registry
}

#[test]
fn registry_uses_shared_harness_ids_and_preserves_duplicate_checks() {
    let fixture: Arc<dyn Harness> =
        Arc::new(Fixture(|_: Execution| -> BoxFuture<'static, Result<()>> {
            Box::pin(async { Ok(()) })
        }));
    let mut registry = Registry::default();
    for id in ["a", "test_harness-09", &"a".repeat(128)] {
        registry.register(id, fixture.clone()).unwrap();
        assert!(registry.register(id, fixture.clone()).is_err());
    }
    for id in ["", "Bad", "0bad", "a/b", "a.b", "aé", &"a".repeat(129)] {
        assert!(registry.register(id, fixture.clone()).is_err());
    }
    assert_eq!(registry.ids().len(), 3);
}
fn command<T>(body: T) -> Command<T> {
    Command::new(RequestKey::new(Uuid::new_v4().to_string()).unwrap(), body)
}
async fn until(mut predicate: impl AsyncFnMut() -> bool) {
    tokio::time::timeout(Duration::from_secs(8), async {
        while !predicate().await {
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .expect("condition did not become true");
}

#[sqlx::test(migrations = "../server/migrations")]
#[ignore = "requires DATABASE_URL; isolated SQLx database"]
async fn empty_build_registers_heartbeats_claims_nothing_and_drains(pool: PgPool) {
    let app = App::new(pool).await;
    let run = app.create().await;
    let worker = app.worker(Registry::default());
    until(async || worker.snapshot.read().await.heartbeat_ok).await;
    let id = worker.snapshot.read().await.worker_id;
    assert_eq!(app.status(run).await, "ready");
    let caps: Vec<String> =
        sqlx::query_scalar("select supported_harnesses from workers where id=$1")
            .bind(id)
            .fetch_one(&app.pool)
            .await
            .unwrap();
    assert!(caps.is_empty());
    worker.shutdown().await;
    let status: String = sqlx::query_scalar("select status from workers where id=$1")
        .bind(id)
        .fetch_one(&app.pool)
        .await
        .unwrap();
    assert_eq!(status, "offline");
}

async fn expired_worker_recovery(pool: PgPool, heartbeat_available: bool) {
    let app = App::new(pool).await;
    let run = app.create().await;
    let fixtures = registry(|_| Box::pin(std::future::pending()));
    let worker = app.worker(fixtures.clone());
    until(async || worker.snapshot.read().await.active_runs == 1).await;
    let old_id = worker.snapshot.read().await.worker_id;
    app.faults.disconnected.store(true, Ordering::SeqCst);
    // Advance durable liveness timestamps instead of sleeping for two minutes.
    // Repeat until reconciliation wins any pre-disconnect in-flight renewal.
    until(async || {
        sqlx::query("update runs set lease_expires_at=clock_timestamp()-interval '1 second' where worker_id=$1")
            .bind(old_id).execute(&app.pool).await.unwrap();
        sqlx::query("update workers set started_at=clock_timestamp()-interval '122 seconds',last_seen_at=clock_timestamp()-interval '121 seconds' where id=$1")
            .bind(old_id).execute(&app.pool).await.unwrap();
        sqlx::query_scalar::<_, String>("select status from workers where id=$1")
            .bind(old_id).fetch_one(&app.pool).await.unwrap() == "offline"
    }).await;
    // Test both recovery paths independently: heartbeat rejection and an
    // offline assignment response while heartbeat transport is unavailable.
    app.faults
        .fail_heartbeat
        .store(!heartbeat_available, Ordering::SeqCst);
    app.faults
        .fail_assignments
        .store(heartbeat_available, Ordering::SeqCst);
    app.faults.disconnected.store(false, Ordering::SeqCst);
    let result = tokio::time::timeout(Duration::from_secs(5), worker.task)
        .await
        .expect("expired worker must exit instead of retrying forever")
        .unwrap();
    assert!(
        matches!(result, Err(platform_runtime_client::Error::WorkerOffline)),
        "the process must report a permanently offline identity so it can restart"
    );
    let snapshot = worker.snapshot.read().await;
    assert!(!snapshot.registered && !snapshot.heartbeat_ok && snapshot.draining);
    assert_eq!(snapshot.active_runs, 0);
    drop(snapshot);
    assert!(["running", "ready"].contains(&app.status(run).await.as_str()));
    let old_status: String = sqlx::query_scalar("select status from workers where id=$1")
        .bind(old_id)
        .fetch_one(&app.pool)
        .await
        .unwrap();
    assert_eq!(old_status, "offline");

    app.faults.fail_heartbeat.store(false, Ordering::SeqCst);
    app.faults.fail_assignments.store(false, Ordering::SeqCst);
    let replacement = app.worker(fixtures);
    until(async || replacement.snapshot.read().await.active_runs == 1).await;
    assert_ne!(replacement.snapshot.read().await.worker_id, old_id);
    let epoch: i64 = sqlx::query_scalar("select lease_epoch from runs where id=$1")
        .bind(run)
        .fetch_one(&app.pool)
        .await
        .unwrap();
    assert_eq!(epoch, 2);
    replacement.shutdown().await;
}

#[sqlx::test(migrations = "../server/migrations")]
#[ignore = "requires DATABASE_URL; isolated SQLx database"]
async fn expired_worker_exits_on_heartbeat_rejection_and_replacement_recovers(pool: PgPool) {
    expired_worker_recovery(pool, true).await;
}

#[sqlx::test(migrations = "../server/migrations")]
#[ignore = "requires DATABASE_URL; isolated SQLx database"]
async fn expired_worker_exits_on_offline_assignments_even_without_heartbeat(pool: PgPool) {
    expired_worker_recovery(pool, false).await;
}

#[sqlx::test(migrations = "../server/migrations")]
#[ignore = "requires DATABASE_URL; isolated SQLx database"]
async fn lost_claim_reply_checkpoint_wait_and_resume_preserve_one_run(pool: PgPool) {
    let app = App::new(pool).await;
    let run = app.create().await;
    app.faults.lose_claim.store(true, Ordering::SeqCst);
    let activations = Arc::new(AtomicUsize::new(0));
    let counter = activations.clone();
    let fixtures = registry(move |execution| {
        let counter = counter.clone();
        Box::pin(async move {
            counter.fetch_add(1, Ordering::SeqCst);
            let ctx = execution.client.context(&ContextQuery::default()).await?;
            let inputs = execution.client.inputs(&SequenceQuery::default()).await?;
            let mut commit = Commit::new(ctx.run.run.version, ctx.session.current_revision);
            commit.input_results = inputs
                .items
                .into_iter()
                .map(|i| InputResult {
                    id: i.id,
                    status: InputStatus::Handled,
                    handling: Default::default(),
                })
                .collect();
            if ctx.checkpoint.is_none() {
                commit.checkpoint = Some(Checkpoint {
                    expected_version: 0,
                    state: json!({"phase":"waiting"}).as_object().unwrap().clone(),
                });
                commit.waits.push(Wait {
                    wait_key: "approval".into(),
                    mode: WaitMode::Any,
                    deadline_at: None,
                    metadata: Default::default(),
                    dependencies: vec![Dependency::Input {
                        input_kind: "approval_response".into(),
                        correlation_key: Some("approve".into()),
                    }],
                });
                commit.disposition = Disposition::Waiting;
            } else {
                assert_eq!(
                    ctx.checkpoint.as_ref().map(|c| &c.state["phase"]),
                    Some(&json!("waiting"))
                );
                let final_id = Uuid::now_v7();
                commit.messages.push(AppendMessage { message_id: final_id, message: serde_json::from_value(json!({"role":"assistant","id":"final","timestamp":0,"model":{"provider":"fixture","id":"test"},"duration_ms":1,"native_message":{},"content":[{"type":"response","response":{"content":"Done"}}],"stop_reason":"stop"})).unwrap() });
                commit.disposition = Disposition::Completed {
                    final_message_id: final_id,
                };
            }
            execution.client.commit(&command(commit)).await?;
            Ok(())
        })
    });
    let worker = app.worker(fixtures.clone());
    until(async || app.status(run).await == "waiting").await;
    until(async || worker.snapshot.read().await.active_runs == 0).await;
    assert_eq!(activations.load(Ordering::SeqCst), 1);
    worker.shutdown().await;
    let worker = app.worker(fixtures);
    app.post(
        &format!("/api/runs/{run}/inputs"),
        json!({"kind":"approval_response","correlation_key":"approve","approved":true}),
    )
    .await;
    until(async || app.status(run).await == "completed").await;
    assert_eq!(activations.load(Ordering::SeqCst), 2);
    let epoch: i64 = sqlx::query_scalar("select lease_epoch from runs where id=$1")
        .bind(run)
        .fetch_one(&app.pool)
        .await
        .unwrap();
    assert_eq!(epoch, 2);
    worker.shutdown().await;
}

#[sqlx::test(migrations = "../server/migrations")]
#[ignore = "requires DATABASE_URL; isolated SQLx database"]
async fn busy_harness_receives_inputs_abort_and_drain_while_heartbeats_continue(pool: PgPool) {
    let app = App::new(pool).await;
    let run = app.create().await;
    let saw_abort = Arc::new(AtomicBool::new(false));
    let saw_input = Arc::new(AtomicBool::new(false));
    let (abort, input) = (saw_abort.clone(), saw_input.clone());
    let worker = app.worker(registry(move |mut execution| {
        let (abort, input) = (abort.clone(), input.clone());
        Box::pin(async move {
            loop {
                let signals = *execution.signals.borrow_and_update();
                if signals.abort_requested {
                    abort.store(true, Ordering::SeqCst);
                }
                if signals.input_generation > 0 {
                    input.store(true, Ordering::SeqCst);
                }
                if signals.draining {
                    let ctx = execution.client.context(&ContextQuery::default()).await?;
                    let mut commit = Commit::new(ctx.run.run.version, ctx.session.current_revision);
                    commit.disposition = Disposition::Ready { available_at: None };
                    execution.client.commit(&command(commit)).await?;
                    return Ok(());
                }
                if execution.signals.changed().await.is_err() {
                    return Ok(());
                }
            }
        })
    }));
    until(async || worker.snapshot.read().await.active_runs == 1).await;
    let first: String = sqlx::query_scalar("select lease_expires_at::text from runs where id=$1")
        .bind(run)
        .fetch_one(&app.pool)
        .await
        .unwrap();
    until(async || {
        sqlx::query_scalar::<_, String>("select lease_expires_at::text from runs where id=$1")
            .bind(run)
            .fetch_one(&app.pool)
            .await
            .unwrap()
            != first
    })
    .await;
    app.post(&format!("/api/runs/{run}/abort"), json!({})).await;
    until(async || saw_abort.load(Ordering::SeqCst) && saw_input.load(Ordering::SeqCst)).await;
    assert_eq!(app.status(run).await, "running");
    let pending: i64 =
        sqlx::query_scalar("select count(*) from run_inputs where run_id=$1 and status='pending'")
            .bind(run)
            .fetch_one(&app.pool)
            .await
            .unwrap();
    assert_eq!(pending, 2); // Supervisor must never acknowledge inputs.
    worker.shutdown().await;
    assert_eq!(app.status(run).await, "ready"); // Drain is not abort acknowledgement.
}

#[sqlx::test(migrations = "../server/migrations")]
#[ignore = "requires DATABASE_URL; isolated SQLx database"]
async fn ownership_loss_cancels_local_execution_without_terminal_write(pool: PgPool) {
    let app = App::new(pool).await;
    let run = app.create().await;
    let worker = app.worker(registry(|_| Box::pin(std::future::pending())));
    until(async || worker.snapshot.read().await.active_runs == 1).await;
    // Draining avoids immediately reacquiring the deliberately expired test run.
    let worker_id = worker.snapshot.read().await.worker_id;
    sqlx::query("update workers set status='draining' where id=$1")
        .bind(worker_id)
        .execute(&app.pool)
        .await
        .unwrap();
    sqlx::query(
        "update runs set lease_expires_at=clock_timestamp()-interval '1 second' where id=$1",
    )
    .bind(run)
    .execute(&app.pool)
    .await
    .unwrap();
    until(async || worker.snapshot.read().await.lost_leases == 1).await;
    assert!(["running", "ready"].contains(&app.status(run).await.as_str()));
    // Worker may already have observed the administrative drain and exited.
    let _ = worker.stop.send(true);
    tokio::time::timeout(Duration::from_secs(5), worker.task)
        .await
        .unwrap()
        .unwrap()
        .unwrap();
}

#[sqlx::test(migrations = "../server/migrations")]
#[ignore = "requires DATABASE_URL; isolated SQLx database"]
async fn panic_is_not_reexecuted_under_same_epoch(pool: PgPool) {
    let app = App::new(pool).await;
    let run = app.create().await;
    let calls = Arc::new(AtomicUsize::new(0));
    let count = calls.clone();
    let worker = app.worker(registry(move |_| {
        let count = count.clone();
        Box::pin(async move {
            count.fetch_add(1, Ordering::SeqCst);
            panic!("test harness crash");
        })
    }));
    until(async || calls.load(Ordering::SeqCst) == 1).await;
    tokio::time::sleep(Duration::from_millis(800)).await;
    assert_eq!(calls.load(Ordering::SeqCst), 1);
    assert_eq!(app.status(run).await, "running");
    worker.shutdown().await;
}

#[sqlx::test(migrations = "../server/migrations")]
#[ignore = "requires DATABASE_URL; isolated SQLx database"]
async fn failed_renewals_enforce_local_deadline_without_platform_response(pool: PgPool) {
    let app = App::new(pool).await;
    let run = app.create().await;
    app.faults.short_lease.store(true, Ordering::SeqCst);
    let worker = app.worker(registry(|_| Box::pin(std::future::pending())));
    until(async || worker.snapshot.read().await.active_runs == 1).await;
    app.faults.fail_heartbeat.store(true, Ordering::SeqCst);
    until(async || worker.snapshot.read().await.lost_leases == 1).await;
    assert_eq!(worker.snapshot.read().await.active_runs, 0);
    assert!(!worker.snapshot.read().await.heartbeat_ok);
    assert_eq!(app.status(run).await, "running");
    worker.shutdown().await;
}

#[sqlx::test(migrations = "../server/migrations")]
#[ignore = "requires DATABASE_URL; isolated SQLx database"]
async fn replicas_respect_capacity_and_replacement_claims_expired_work(pool: PgPool) {
    let app = App::new(pool).await;
    let first_run = app.create().await;
    let second_run = app.create().await;
    let fixtures = registry(|_| Box::pin(std::future::pending()));
    let first = app.worker(fixtures.clone());
    let second = app.worker(fixtures.clone());
    until(async || {
        first.snapshot.read().await.active_runs == 1
            && second.snapshot.read().await.active_runs == 1
    })
    .await;
    let first_id = first.snapshot.read().await.worker_id;
    let first_owned: Uuid = sqlx::query_scalar("select id from runs where worker_id=$1")
        .bind(first_id)
        .fetch_one(&app.pool)
        .await
        .unwrap();
    let owners: i64 =
        sqlx::query_scalar("select count(distinct worker_id) from runs where id=any($1)")
            .bind(vec![first_run, second_run])
            .fetch_one(&app.pool)
            .await
            .unwrap();
    assert_eq!(owners, 2);
    first.task.abort(); // Simulated process loss: no graceful disposition.
    let _ = first.task.await;
    sqlx::query(
        "update runs set lease_expires_at=clock_timestamp()-interval '1 second' where id=$1",
    )
    .bind(first_owned)
    .execute(&app.pool)
    .await
    .unwrap();
    let replacement = app.worker(fixtures);
    until(async || replacement.snapshot.read().await.active_runs == 1).await;
    let epoch: i64 = sqlx::query_scalar("select lease_epoch from runs where id=$1")
        .bind(first_owned)
        .fetch_one(&app.pool)
        .await
        .unwrap();
    assert_eq!(epoch, 2);
    let queued = app.create().await;
    tokio::time::sleep(Duration::from_millis(200)).await;
    assert_eq!(app.status(queued).await, "ready");
    tokio::join!(second.shutdown(), replacement.shutdown());
}
