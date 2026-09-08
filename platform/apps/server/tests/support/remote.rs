//! Real shell supervisor behind a local gateway; no provider or external host.
use super::*;
use axum::{
    extract::{Path, State},
    http::{HeaderMap, StatusCode},
    response::{IntoResponse, Response},
    routing::post,
};
use execution_core::{ExecutionHostId, ExecutionRuntime, OperationContext, RootId};
use execution_supervisor_core::{
    SupervisorConfig, SupervisorLimits, SupervisorRoot, SupervisorRuntime,
};
use execution_wire::{Operation, RequestEnvelope, dispatch_request};
use platform_runtime_client::{
    Command as Cmd, RequestKey,
    types::{self as t, capabilities as c},
};
use std::{
    collections::BTreeMap,
    sync::{
        Arc,
        atomic::{AtomicBool, AtomicUsize, Ordering},
    },
};
use tokio::sync::Mutex;

struct Gateway {
    root: tempfile::TempDir,
    hosts: Mutex<BTreeMap<Uuid, (Arc<SupervisorRuntime>, Value)>>,
    keys: Mutex<BTreeMap<String, (Value, Uuid)>>,
    lose_create: AtomicBool,
    lose_start: AtomicBool,
    block_cancel: AtomicBool,
    block_delete: AtomicBool,
    starts: AtomicUsize,
    snapshot_roots: Mutex<BTreeMap<Uuid, std::path::PathBuf>>,
    pause_start: AtomicBool,
    start_entered: tokio::sync::Notify,
    release_start: tokio::sync::Notify,
}
impl Gateway {
    async fn add(&self, id: Uuid, kind: &str, metadata: Value, source: Value) -> Value {
        let root = if let Some(snapshot) = source["snapshot_id"].as_str() {
            self.snapshot_roots
                .lock()
                .await
                .get(&snapshot.parse().unwrap())
                .cloned()
                .unwrap_or_else(|| self.root.path().join(id.to_string()))
        } else {
            self.root.path().join(id.to_string())
        };
        std::fs::create_dir_all(&root).unwrap();
        let host = Arc::new(
            SupervisorRuntime::new(SupervisorConfig {
                host_id: ExecutionHostId::new(id.to_string()).unwrap(),
                state_directory: self.root.path().join(format!("state-{id}")),
                roots: vec![SupervisorRoot {
                    id: RootId::new("work").unwrap(),
                    name: "Work".into(),
                    path: root,
                    read_only: false,
                }],
                limits: SupervisorLimits {
                    termination_grace_period: Duration::from_millis(25),
                    ..Default::default()
                },
            })
            .await
            .unwrap(),
        );
        let descriptor = host.descriptor();
        let now = chrono::Utc::now();
        let value = json!({"id":id,"kind":kind,"name":"Test host","state":"ready","desired_state":"ready","status_retryable":false,"roots":descriptor.roots,"descriptor":descriptor,"metadata":metadata,"revision":1,"created_at":now,"updated_at":now,"e2b":if kind=="e2b" {json!({"e2b_account_id":Uuid::new_v4(),"e2b_sandbox_id":"fake","source":source,"timeout_seconds":3600,"network_access":true})} else {Value::Null}});
        self.hosts.lock().await.insert(id, (host, value.clone()));
        value
    }
}
async fn get_host(State(g): State<Arc<Gateway>>, Path(id): Path<Uuid>) -> Response {
    match g.hosts.lock().await.get(&id) {
        Some((_, v)) => Json(v.clone()).into_response(),
        None => StatusCode::NOT_FOUND.into_response(),
    }
}
async fn delete_host(State(g): State<Arc<Gateway>>, Path(id): Path<Uuid>) -> Response {
    if g.block_delete.load(Ordering::SeqCst) {
        return StatusCode::BAD_GATEWAY.into_response();
    }
    let mut hosts = g.hosts.lock().await;
    let (_, value) = hosts.get_mut(&id).unwrap();
    value["state"] = json!("deleted");
    value["desired_state"] = json!("deleted");
    Json(value.clone()).into_response()
}
async fn create_host(
    State(g): State<Arc<Gateway>>,
    headers: HeaderMap,
    Json(request): Json<Value>,
) -> Response {
    let key = headers["idempotency-key"].to_str().unwrap();
    let mut keys = g.keys.lock().await;
    let id = if let Some((old, id)) = keys.get(key) {
        assert_eq!(old, &request);
        *id
    } else {
        let id = Uuid::now_v7();
        g.add(
            id,
            "e2b",
            request["metadata"].clone(),
            request["source"].clone(),
        )
        .await;
        keys.insert(key.into(), (request.clone(), id));
        id
    };
    if g.lose_create.swap(false, Ordering::SeqCst) {
        return StatusCode::BAD_GATEWAY.into_response();
    }
    Json(g.hosts.lock().await[&id].1.clone()).into_response()
}
async fn operations(
    State(g): State<Arc<Gateway>>,
    Path(id): Path<Uuid>,
    Json(request): Json<RequestEnvelope>,
) -> Response {
    let host = g.hosts.lock().await.get(&id).unwrap().0.clone();
    let start = matches!(request.operation, Operation::ProcessStart(_));
    if matches!(request.operation, Operation::ProcessTerminate(_))
        && g.block_cancel.load(Ordering::SeqCst)
    {
        return StatusCode::BAD_GATEWAY.into_response();
    }
    if start {
        g.starts.fetch_add(1, Ordering::SeqCst);
    }
    let result = dispatch_request(host.as_ref(), &OperationContext::new(), request).await;
    if start && g.pause_start.swap(false, Ordering::SeqCst) {
        g.start_entered.notify_one();
        g.release_start.notified().await;
    }
    if start && g.lose_start.swap(false, Ordering::SeqCst) {
        return StatusCode::BAD_GATEWAY.into_response();
    }
    Json(result).into_response()
}
async fn setup(pool: PgPool) -> (Fixture, Arc<Gateway>, Uuid, String) {
    let gateway = Arc::new(Gateway {
        root: tempfile::tempdir().unwrap(),
        hosts: Mutex::new(BTreeMap::new()),
        keys: Mutex::new(BTreeMap::new()),
        lose_create: AtomicBool::new(false),
        lose_start: AtomicBool::new(false),
        block_cancel: AtomicBool::new(false),
        block_delete: AtomicBool::new(false),
        starts: AtomicUsize::new(0),
        snapshot_roots: Mutex::new(BTreeMap::new()),
        pause_start: AtomicBool::new(false),
        start_entered: tokio::sync::Notify::new(),
        release_start: tokio::sync::Notify::new(),
    });
    let host = Uuid::now_v7();
    let value = gateway
        .add(host, "registered", json!({}), Value::Null)
        .await;
    let cwd = value["roots"][0]["native_path"]
        .as_str()
        .unwrap()
        .to_string();
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}", listener.local_addr().unwrap());
    let app = Router::new()
        .route("/v1/hosts", post(create_host))
        .route("/v1/hosts/{id}", get(get_host).delete(delete_host))
        .route("/v1/hosts/{id}/operations", post(operations))
        .with_state(gateway.clone());
    let task = tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });
    let mut f = Fixture::with_remote_content(pool, None, None, Some(&url)).await;
    f.tasks.push(task);
    sqlx::query("update project_environments set machine_id=$1,workspace_root=$2 where id=$3")
        .bind(host)
        .bind(&cwd)
        .bind(f.environment)
        .execute(&f.pool)
        .await
        .unwrap();
    (f, gateway, host, cwd)
}
fn ok(r: Value) -> Value {
    assert_eq!(r["status"], 200, "{r}");
    r["body"].clone()
}

#[sqlx::test(migrations = "./migrations")]
#[ignore = "requires local PostgreSQL and a built sites-service binary"]
async fn remote_server_takeover_fences_stale_start_acknowledgements(pool: PgPool) {
    let (f, g, host, cwd) = setup(pool).await;
    enable(&f).await;
    let accepted = ok(f
        .sdk(
            "execution.bash",
            mutation(
                json!({"hostId":host,"workdir":cwd,"command":"printf x >> once.txt; cat once.txt"}),
                "once",
            ),
        )
        .await);
    tick(&f).await; // Prepared identity.
    tick(&f).await; // Submission intent.
    g.pause_start.store(true, Ordering::SeqCst);
    let old_service = f.runtime.clone();
    sqlx::query(
        "update platform_remote_operations set next_attempt_at=clock_timestamp() where id=$1",
    )
    .bind(id(&accepted))
    .execute(&f.pool)
    .await
    .unwrap();
    let old = tokio::spawn(async move { old_service.reconcile_remote_once().await.unwrap() });
    tokio::time::timeout(Duration::from_secs(5), g.start_entered.notified())
        .await
        .unwrap();
    sqlx::query("update platform_remote_operations set processor_expires_at=clock_timestamp()-interval '1 second' where id=$1").bind(id(&accepted)).execute(&f.pool).await.unwrap();
    tick(&f).await; // Replacement replays the same start while the old ACK is held.
    let finished = done(&f, id(&accepted)).await;
    assert_eq!(finished["status"], "completed");
    g.release_start.notify_one();
    assert!(old.await.unwrap());
    assert_eq!(
        ok(f.sdk("execution.get", json!([accepted["id"]])).await),
        finished
    );
    assert_eq!(
        ok(f.sdk("execution.output", json!([accepted["id"]])).await)["output"],
        "x"
    );
    assert_eq!(
        std::fs::read_to_string(std::path::Path::new(&cwd).join("once.txt")).unwrap(),
        "x"
    );
}

#[sqlx::test(migrations = "./migrations")]
#[ignore = "requires local PostgreSQL and a built sites-service binary"]
async fn remote_supervisor_generation_change_never_restarts_prepared_commands(pool: PgPool) {
    let (f, g, host, cwd) = setup(pool).await;
    enable(&f).await;
    let accepted = ok(f
        .sdk(
            "execution.bash",
            mutation(
                json!({"hostId":host,"workdir":cwd,"command":"printf must-not-run"}),
                "generation",
            ),
        )
        .await);
    tick(&f).await;
    g.add(host, "registered", json!({}), Value::Null).await;
    let lost = done(&f, id(&accepted)).await;
    assert_eq!(lost["status"], "lost");
    assert_eq!(lost["cancellationConfirmed"], false);
    assert_eq!(g.starts.load(Ordering::SeqCst), 0);
}

#[sqlx::test(migrations = "./migrations")]
#[ignore = "requires local PostgreSQL and a built sites-service binary"]
async fn remote_harness_sandbox_binding_preserves_files_and_rejects_output_only_authority(
    pool: PgPool,
) {
    let (mut f, g, _, cwd) = setup(pool).await;
    enable(&f).await;
    let started = ok(f
        .sdk("sessions.start", mutation(f.start(), "evaluation"))
        .await);
    let source: Uuid = serde_json::from_value(started["runId"].clone()).unwrap();
    let (client, _) = super::capabilities::worker(&mut f).await;
    let run = super::capabilities::claim(&client, source).await;
    let env = Uuid::now_v7();
    let snapshot = Uuid::now_v7();
    g.snapshot_roots
        .lock()
        .await
        .insert(snapshot, cwd.clone().into());
    sqlx::query("insert into project_environments(id,project_id,name,type,snapshot_id,workspace_root,path) values($1,$2,'Evaluation','sandbox',$3,$4,'.')")
        .bind(env).bind(f.project).bind(snapshot).bind(&cwd).execute(&f.pool).await.unwrap();
    let host = Uuid::now_v7();
    g.add(
        host,
        "e2b",
        json!({"platform_project_id":Uuid::new_v4(),"platform_session_id":started["sessionId"]}),
        json!({"type":"snapshot","snapshot_id":snapshot}),
    )
    .await;
    let output = t::ExecutionWorkspace {
        host_id: host,
        workspace_root: cwd.clone(),
        path: ".".into(),
        environment_id: Some(env),
        sandbox_id: None,
    };
    run.publish_output(&Cmd::new(
        RequestKey::new("output").unwrap(),
        t::PublishRunOutput {
            name: "workspace".into(),
            output: t::OutputValue::ExecutionWorkspace(output),
        },
    ))
    .await
    .unwrap();
    let bind = Cmd::new(
        RequestKey::new("bind").unwrap(),
        c::BindHarnessWorkspace {
            environment_id: env,
            host_id: host,
        },
    );
    assert!(run.bind_workspace(&bind).await.is_err());
    let args = json!({"hostId":host,"workdir":cwd,"command":"printf from-evaluated-sandbox > evaluated.txt; cat evaluated.txt"});
    assert_eq!(
        f.sdk("execution.bash", mutation(args.clone(), "unbound"))
            .await["status"],
        400
    );
    g.hosts.lock().await.get_mut(&host).unwrap().1["metadata"]["platform_project_id"] =
        json!(f.project);
    let bound = run.bind_workspace(&bind).await.unwrap();
    let sandbox = bound
        .sandbox_id
        .expect("binding returns a usable lifecycle handle");
    tick(&f).await;
    let accepted = ok(f.sdk("execution.bash", mutation(args, "evaluate")).await);
    assert_eq!(done(&f, id(&accepted)).await["status"], "completed");
    let context = run.context(&t::ContextQuery::default()).await.unwrap();
    let mut finish = t::Commit::new(context.run.run.version, context.session.current_revision);
    finish.disposition = t::Disposition::Failed {
        error: json!({"code":"EVAL_FAILED","message":"Preserve files for verifier"})
            .as_object()
            .unwrap()
            .clone(),
    };
    run.commit(&Cmd::new(RequestKey::new("finish").unwrap(), finish))
        .await
        .unwrap();
    let published =
        ok(f.sdk("runs.outputs", json!([source])).await)["items"][0]["output"]["value"].clone();
    let verifier=ok(f.sdk("execution.bash",mutation(json!({"hostId":published["host_id"],"workdir":published["workspace_root"],"command":"cat evaluated.txt"}),"verify-sandbox")).await);
    assert_eq!(done(&f, id(&verifier)).await["status"], "completed");
    assert_eq!(
        ok(f.sdk("execution.output", json!([verifier["id"]])).await)["output"],
        "from-evaluated-sandbox"
    );
    assert!(
        g.keys.lock().await.is_empty(),
        "binding and verification allocate no sandbox"
    );
    ok(f.sdk(
        "sandboxes.terminate",
        mutation(json!({"sandboxId":sandbox}), "cleanup"),
    )
    .await);
    tick(&f).await;
    assert_eq!(
        ok(f.sdk("sandboxes.get", json!([sandbox])).await)["terminationConfirmed"],
        true
    );
}

#[sqlx::test(migrations = "./migrations")]
#[ignore = "requires local PostgreSQL and a built sites-service binary"]
async fn remote_ambiguous_submission_cancellation_never_launches_work(pool: PgPool) {
    let (f, g, host, cwd) = setup(pool).await;
    enable(&f).await;
    let accepted = ok(f
        .sdk(
            "execution.bash",
            mutation(
                json!({"hostId":host,"workdir":cwd,"command":"sleep 20","timeoutMs":25000}),
                "ambiguous",
            ),
        )
        .await);
    tick(&f).await;
    tick(&f).await;
    g.lose_start.store(true, Ordering::SeqCst);
    tick(&f).await; // Process exists but its acknowledgement was lost.
    ok(f.sdk(
        "execution.cancel",
        mutation(json!({"executionId":accepted["id"]}), "stop"),
    )
    .await);
    assert_eq!(done(&f, id(&accepted)).await["cancellationConfirmed"], true);
    assert_eq!(g.starts.load(Ordering::SeqCst), 1);

    let never_sent = ok(f
        .sdk(
            "execution.bash",
            mutation(
                json!({"hostId":host,"workdir":cwd,"command":"printf must-not-run"}),
                "never-sent",
            ),
        )
        .await);
    tick(&f).await;
    tick(&f).await; // Intent committed, server disappeared before start dispatch.
    ok(f.sdk(
        "execution.cancel",
        mutation(json!({"executionId":never_sent["id"]}), "stop-unsent"),
    )
    .await);
    tick(&f).await;
    let unknown = ok(f.sdk("execution.get", json!([never_sent["id"]])).await);
    assert_eq!(unknown["cancellationRequested"], true);
    assert_eq!(unknown["cancellationConfirmed"], false);
    assert!(unknown["finishedAt"].is_null());
    assert_eq!(
        g.starts.load(Ordering::SeqCst),
        1,
        "must not replay start merely to cancel it"
    );
}
fn mutation(input: Value, key: &str) -> Value {
    json!([input,{"idempotencyKey":key}])
}
async fn enable(f: &Fixture) {
    f.request(Method::PUT,&format!("/internal/site-access/{}",f.project),ADMIN,
        json!({"token":TOKEN,"enabled":true,"executionEnabled":true,"harnesses":[{"id":HARNESS,"environmentMode":"single"}],"accountIds":[f.account]}),200).await;
}
async fn tick(f: &Fixture) {
    sqlx::query("update platform_remote_operations set next_attempt_at=next_attempt_at-interval '1 minute' where finished_at is null").execute(&f.pool).await.unwrap();
    f.runtime.reconcile_remote_once().await.unwrap();
}
async fn done(f: &Fixture, id: Uuid) -> Value {
    for _ in 0..40 {
        tick(f).await;
        let r = ok(f.sdk("execution.get", json!([id])).await);
        if !r["finishedAt"].is_null() {
            return r;
        }
    }
    panic!("execution did not complete")
}
fn id(v: &Value) -> Uuid {
    serde_json::from_value(v["id"].clone()).unwrap()
}

#[sqlx::test(migrations = "./migrations")]
#[ignore = "requires local PostgreSQL and a built sites-service binary"]
async fn remote_commands_are_durable_and_verify_published_workspace(pool: PgPool) {
    let (mut f, g, host, cwd) = setup(pool).await;
    let args = json!({"hostId":host,"workdir":cwd,"command":"printf evaluated > result.txt; printf 'αβγ'","timeoutMs":5000});
    assert_eq!(
        f.sdk("execution.bash", mutation(args.clone(), "denied"))
            .await["status"],
        400
    );
    enable(&f).await;
    g.lose_start.store(true, Ordering::SeqCst);
    let (a, b) = tokio::join!(
        f.sdk("execution.bash", mutation(args.clone(), "build")),
        f.sdk("execution.bash", mutation(args.clone(), "build"))
    );
    let accepted = ok(a);
    assert_eq!(accepted, ok(b));
    assert_eq!(accepted["status"], "pending");
    let execution = id(&accepted);
    // Completed backend invocations have not submitted a command; the server owns it.
    assert_eq!(g.starts.load(Ordering::SeqCst), 0);
    let result = done(&f, execution).await;
    assert_eq!(result["status"], "completed");
    assert_eq!(result["exitCode"], 0);
    assert!(
        g.starts.load(Ordering::SeqCst) >= 2,
        "lost reply replayed same prepared identity"
    );
    let first = ok(f
        .sdk("execution.output", json!([execution,{"limitBytes":2}]))
        .await);
    assert_eq!(first["output"], "α");
    let rest = ok(f
        .sdk(
            "execution.output",
            json!([execution,{"cursor":first["nextCursor"]}]),
        )
        .await);
    assert_eq!(rest["output"], "βγ");
    // Publish the actual evaluated host, finish that run, and verify from a new caller.
    let started = ok(f
        .sdk("sessions.start", mutation(f.start(), "evaluated"))
        .await);
    let source: Uuid = serde_json::from_value(started["runId"].clone()).unwrap();
    let (client, _) = super::capabilities::worker(&mut f).await;
    let run = super::capabilities::claim(&client, source).await;
    let bound = run
        .bind_workspace(&Cmd::new(
            RequestKey::new("bind").unwrap(),
            c::BindHarnessWorkspace {
                environment_id: f.environment,
                host_id: host,
            },
        ))
        .await
        .unwrap();
    run.publish_output(&Cmd::new(
        RequestKey::new("out").unwrap(),
        t::PublishRunOutput {
            name: "workspace".into(),
            output: t::OutputValue::ExecutionWorkspace(bound),
        },
    ))
    .await
    .unwrap();
    let context = run.context(&t::ContextQuery::default()).await.unwrap();
    let mut commit = t::Commit::new(context.run.run.version, context.session.current_revision);
    commit.disposition = t::Disposition::Failed {
        error: json!({"kind":"evaluation_done"})
            .as_object()
            .unwrap()
            .clone(),
    };
    run.commit(&Cmd::new(RequestKey::new("finish").unwrap(), commit))
        .await
        .unwrap();
    let outputs = ok(f.sdk("runs.outputs", json!([source])).await);
    let actual = &outputs["items"][0]["output"]["value"];
    let verify=ok(f.sdk("execution.bash",mutation(json!({"hostId":actual["host_id"],"workdir":actual["workspace_root"],"command":"cat result.txt"}),"verify")).await);
    done(&f, id(&verify)).await;
    assert_eq!(
        ok(f.sdk("execution.output", json!([verify["id"]])).await)["output"],
        "evaluated"
    );
    assert_eq!(
        g.keys.lock().await.len(),
        0,
        "verification creates no replacement sandbox"
    );
    assert_eq!(
        ok(f.sdk("execution.bash", mutation(args, "build")).await),
        accepted,
        "receipt preserves initial handle"
    );
}

#[sqlx::test(migrations = "./migrations")]
#[ignore = "requires local PostgreSQL and a built sites-service binary"]
async fn remote_sandbox_creation_recovery_expiry_and_project_binding(pool: PgPool) {
    let (f, g, _, cwd) = setup(pool).await;
    enable(&f).await;
    let env = Uuid::now_v7();
    let snapshot = Uuid::now_v7();
    g.snapshot_roots
        .lock()
        .await
        .insert(snapshot, cwd.clone().into());
    sqlx::query("insert into project_environments(id,project_id,name,type,snapshot_id,workspace_root,path) values($1,$2,'Snapshot','sandbox',$3,$4,'.')").bind(env).bind(f.project).bind(snapshot).bind(&cwd).execute(&f.pool).await.unwrap();
    g.lose_create.store(true, Ordering::SeqCst);
    let accepted = ok(f
        .sdk(
            "sandboxes.createFromSnapshot",
            mutation(json!({"environmentId":env}), "sandbox"),
        )
        .await);
    for _ in 0..5 {
        tick(&f).await;
    }
    let ready = ok(f.sdk("sandboxes.get", json!([accepted["id"]])).await);
    assert_eq!(ready["status"], "ready");
    assert_eq!(g.keys.lock().await.len(), 1);
    assert_eq!(ready["workspace"]["environment_id"], json!(env));
    // Expiry revokes new commands immediately, but is not deletion confirmation.
    sqlx::query("update platform_remote_operations set expires_at=clock_timestamp()-interval '1 second' where id=$1").bind(id(&accepted)).execute(&f.pool).await.unwrap();
    g.block_delete.store(true, Ordering::SeqCst);
    tick(&f).await;
    let pending = ok(f.sdk("sandboxes.get", json!([accepted["id"]])).await);
    assert_eq!(pending["terminationRequested"], true);
    assert_eq!(pending["terminationConfirmed"], false);
    assert_eq!(
        f.sdk(
            "execution.bash",
            mutation(
                json!({"hostId":ready["workspace"]["host_id"],"workdir":cwd,"command":"true"}),
                "expired"
            )
        )
        .await["status"],
        400
    );
    g.block_delete.store(false, Ordering::SeqCst);
    tick(&f).await;
    let expired = ok(f.sdk("sandboxes.get", json!([accepted["id"]])).await);
    assert_eq!(expired["status"], "expired");
    assert_eq!(expired["terminationConfirmed"], true);
    assert_eq!(
        f.sdk(
            "sandboxes.createFromSnapshot",
            mutation(json!({"environmentId":f.foreign_environment}), "foreign")
        )
        .await["status"],
        400
    );
    for method in ["sandboxes.get", "execution.get", "execution.output"] {
        assert_eq!(f.sdk(method, json!([Uuid::new_v4()])).await["status"], 400);
    }
}

#[sqlx::test(migrations = "./migrations")]
#[ignore = "requires local PostgreSQL and a built sites-service binary"]
async fn remote_cancellation_is_confirmed_only_after_terminal_observation(pool: PgPool) {
    let (f, g, host, cwd) = setup(pool).await;
    enable(&f).await;
    let cancelled = ok(f
        .sdk(
            "execution.bash",
            mutation(
                json!({"hostId":host,"workdir":cwd,"command":"printf should-not-run"}),
                "before-start",
            ),
        )
        .await);
    ok(f.sdk(
        "execution.cancel",
        mutation(json!({"executionId":cancelled["id"]}), "cancel-early"),
    )
    .await);
    assert_eq!(
        done(&f, id(&cancelled)).await["cancellationConfirmed"],
        true
    );
    assert_eq!(g.starts.load(Ordering::SeqCst), 0);
    let active=ok(f.sdk("execution.bash",mutation(json!({"hostId":host,"workdir":cwd,"command":"printf started; sleep 20","timeoutMs":25000}),"long")).await);
    for _ in 0..4 {
        tick(&f).await;
    }
    g.block_cancel.store(true, Ordering::SeqCst);
    let requested = ok(f
        .sdk(
            "execution.cancel",
            mutation(json!({"executionId":active["id"]}), "cancel-long"),
        )
        .await);
    assert_eq!(requested["cancellationRequested"], true);
    assert_eq!(requested["cancellationConfirmed"], false);
    tick(&f).await;
    let uncertain = ok(f.sdk("execution.get", json!([active["id"]])).await);
    assert_eq!(uncertain["cancellationConfirmed"], false);
    assert!(uncertain["finishedAt"].is_null());
    g.block_cancel.store(false, Ordering::SeqCst);
    let completed = done(&f, id(&active)).await;
    assert_eq!(completed["cancellationConfirmed"], true);
    assert_eq!(completed["status"], "cancelled");
    assert_eq!(
        ok(f.sdk("execution.output", json!([active["id"]])).await)["output"],
        "started"
    );
}

#[sqlx::test(migrations = "./migrations")]
#[ignore = "requires local PostgreSQL and a built sites-service binary"]
async fn remote_agent_replacement_and_output_bounds(pool: PgPool) {
    let (mut f, _, host, cwd) = setup(pool).await;
    enable(&f).await;
    let started = ok(f.sdk("sessions.start", mutation(f.start(), "source")).await);
    let source: Uuid = serde_json::from_value(started["runId"].clone()).unwrap();
    let (client, url) = super::capabilities::worker(&mut f).await;
    let run = super::capabilities::claim(&client, source).await;
    let command = Cmd::new(
        RequestKey::new("bulk").unwrap(),
        c::Bash {
            host_id: host,
            workdir: cwd,
            command: "head -c 1100000 /dev/zero | tr '\\000' x".into(),
            timeout_ms: Some(10000),
        },
    );
    let accepted = run.platform().bash(&command).await.unwrap();
    tick(&f).await;
    sqlx::query(
        "update runs set lease_expires_at=clock_timestamp()-interval '1 second' where id=$1",
    )
    .bind(source)
    .execute(&f.pool)
    .await
    .unwrap();
    let replacement = platform_runtime_client::PlatformClient::new(
        &url,
        Uuid::new_v4(),
        SERVICE,
        Default::default(),
    )
    .unwrap();
    replacement
        .register(
            ADMIN,
            &platform_runtime_client::WorkerRegistration {
                build_id: "replacement".into(),
                supported_harnesses: vec![HARNESS.into()],
                capacity: 1,
            },
        )
        .await
        .unwrap();
    let next = super::capabilities::claim(&replacement, source).await;
    assert_eq!(
        next.platform().bash(&command).await.unwrap().id,
        accepted.id
    );
    assert!(run.platform().get_execution(accepted.id).await.is_err());
    let completed = done(&f, accepted.id).await;
    assert_eq!(completed["truncated"], true);
    assert_eq!(completed["outputBytes"], 1100000);
    let page = next
        .platform()
        .execution_output(
            accepted.id,
            &c::ExecutionOutputOptions {
                cursor: None,
                limit_bytes: Some(65536),
            },
        )
        .await
        .unwrap();
    assert_eq!(page.output.len(), 65536);
    assert!(page.truncated);
    assert!(
        next.platform()
            .execution_output(
                accepted.id,
                &c::ExecutionOutputOptions {
                    cursor: Some(format!("{}:0", Uuid::new_v4())),
                    limit_bytes: None
                }
            )
            .await
            .is_err()
    );
    let stored: i64 = sqlx::query_scalar(
        "select octet_length(output)::bigint from platform_remote_operations where id=$1",
    )
    .bind(accepted.id)
    .fetch_one(&f.pool)
    .await
    .unwrap();
    assert_eq!(stored, 1048576);
}
