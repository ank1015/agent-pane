//! Session storage through real HTTP/SDK calls and isolated PostgreSQL databases.
use platform_runtime_client::{
    ClientConfig, Command, Error, PlatformClient, RequestKey, RunClient, WorkerRegistration,
    types::*,
};
use platform_server::runtime::{RuntimeService, router, worker_router};
use serde_json::{Value, json};
use sqlx::PgPool;
use uuid::Uuid;

const BOOT: &str = "session-state-bootstrap-01234567890123456789";
const TOKEN: &str = "session-state-worker-token-01234567890123456789";
struct App {
    pool: PgPool,
    url: String,
    project: Uuid,
    client: PlatformClient,
    task: tokio::task::JoinHandle<()>,
}
impl Drop for App {
    fn drop(&mut self) {
        self.task.abort();
    }
}
impl App {
    async fn new(pool: PgPool) -> Self {
        let project = Uuid::now_v7();
        sqlx::query("insert into projects(project_id,name) values($1,'Session state tests')")
            .bind(project)
            .execute(&pool)
            .await
            .unwrap();
        sqlx::query("insert into harnesses(id,name) values('test','Test')")
            .execute(&pool)
            .await
            .unwrap();
        let runtime = RuntimeService::new(pool.clone());
        let app = router(runtime.clone()).merge(worker_router(runtime, BOOT));
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("http://{}", listener.local_addr().unwrap());
        let task = tokio::spawn(async move {
            axum::serve(listener, app).await.unwrap();
        });
        let client = Self::worker(&url).await;
        Self {
            pool,
            url,
            project,
            client,
            task,
        }
    }
    async fn worker(url: &str) -> PlatformClient {
        let client =
            PlatformClient::new(url, Uuid::new_v4(), TOKEN, ClientConfig::default()).unwrap();
        client
            .register(
                BOOT,
                &WorkerRegistration {
                    build_id: "test".into(),
                    supported_harnesses: vec!["test".into()],
                    capacity: 8,
                },
            )
            .await
            .unwrap();
        client
    }
    async fn post(&self, path: &str, body: Value) -> Value {
        reqwest::Client::new()
            .post(format!("{}{path}", self.url))
            .header("idempotency-key", Uuid::new_v4().to_string())
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
    async fn start(&self) -> RunClient {
        let created = self.post(&format!("/api/projects/{}/sessions", self.project),
            json!({"harness_id":"test","initial_run":{"expected_session_revision":0,"input":user()}})).await;
        self.claim(&self.client, uuid(&created["run"]["id"])).await
    }
    async fn claim(&self, worker: &PlatformClient, id: Uuid) -> RunClient {
        let claim = worker.claim(&command(Claim { limit: 8 })).await.unwrap();
        let lease = claim
            .items
            .iter()
            .find(|a| a.run.run.id == id)
            .unwrap()
            .lease();
        worker.run(lease).unwrap()
    }
}
fn uuid(v: &Value) -> Uuid {
    serde_json::from_value(v.clone()).unwrap()
}
fn user() -> Value {
    json!({"role":"user","id":"request","timestamp":0,"content":[{"type":"text","content":"Work"}]})
}
fn assistant() -> Message {
    serde_json::from_value(json!({"role":"assistant","id":"response","timestamp":0,"model":{"provider":"test","id":"model"},"duration_ms":1,"native_message":{},"content":[{"type":"response","response":{"content":"Done"}}],"stop_reason":"stop"})).unwrap()
}
fn command<T>(body: T) -> Command<T> {
    Command::new(RequestKey::new(Uuid::new_v4().to_string()).unwrap(), body)
}
fn set(ns: &str, key: &str, version: i64, value: Value) -> SessionStateWrite {
    SessionStateWrite {
        namespace: ns.into(),
        key: key.into(),
        expected_version: version,
        mutation: SessionStateMutation::Set {
            value: value.as_object().unwrap().clone(),
        },
    }
}
fn query(ns: &str, key: Option<&str>) -> SessionStateQuery {
    SessionStateQuery {
        namespace: ns.into(),
        key: key.map(str::to_owned),
        ..Default::default()
    }
}
async fn base(run: &RunClient) -> Commit {
    let ctx = run.context(&ContextQuery::default()).await.unwrap();
    Commit::new(ctx.run.run.version, ctx.session.current_revision)
}
fn conflict<T>(result: Result<T, Error>, expected: ConflictCode) {
    assert_eq!(
        result.err().expect("expected conflict").conflict(),
        Some(expected)
    );
}

#[sqlx::test(migrations = "./migrations")]
#[ignore = "requires DATABASE_URL; isolated SQLx database"]
async fn session_state_survives_followups_but_not_forks_or_other_sessions(pool: PgPool) {
    let app = App::new(pool).await;
    let run = app.start().await;
    let context = run.context(&ContextQuery::default()).await.unwrap();
    let mut c = base(&run).await;
    c.checkpoint = Some(Checkpoint {
        expected_version: 0,
        state: json!({"phase":"working"}).as_object().unwrap().clone(),
    });
    c.session_state = vec![set(
        "codex.exec",
        "12",
        0,
        json!({"execution_id":"durable-handle","cursor":8}),
    )];
    c.input_results = run
        .inputs(&SequenceQuery::default())
        .await
        .unwrap()
        .items
        .into_iter()
        .map(|i| InputResult {
            id: i.id,
            status: InputStatus::Handled,
            handling: Default::default(),
        })
        .collect();
    let final_id = Uuid::now_v7();
    c.messages.push(AppendMessage {
        message_id: final_id,
        message: assistant(),
    });
    let reply = run.commit(&command(c)).await.unwrap();
    assert_eq!(reply.session_revision, 1);
    assert_eq!(reply.session_state[0].version, 1);
    assert_eq!(reply.checkpoint.unwrap().state["phase"], "working");

    let child = run
        .create_child(&command(Child {
            harness_id: None,
            title: None,
            fork_at_revision: Some(1),
            initial_run: StartRun {
                input: serde_json::from_value(user()).unwrap(),
                expected_session_revision: 1,
                config_override: Default::default(),
            },
        }))
        .await
        .unwrap();
    let child_run = app.claim(&app.client, child.run.id).await;
    assert!(
        child_run
            .session_state(&query("codex.exec", None))
            .await
            .unwrap()
            .items
            .is_empty()
    );
    assert!(
        child_run
            .context(&ContextQuery::default())
            .await
            .unwrap()
            .checkpoint
            .is_none()
    );
    let mut c = base(&child_run).await;
    c.session_state.push(set(
        "codex.exec",
        "12",
        0,
        json!({"execution_id":"child-only"}),
    ));
    child_run.commit(&command(c)).await.unwrap();
    assert_eq!(
        run.session_state(&query("codex.exec", Some("12")))
            .await
            .unwrap()
            .items[0]
            .value
            .as_ref()
            .unwrap()["execution_id"],
        "durable-handle"
    );
    let other = app.start().await;
    assert!(
        other
            .session_state(&query("codex.exec", None))
            .await
            .unwrap()
            .items
            .is_empty()
    );

    let mut c = base(&run).await;
    c.disposition = Disposition::Completed {
        final_message_id: final_id,
    };
    run.commit(&command(c)).await.unwrap();
    let next = app
        .post(
            &format!("/api/sessions/{}/runs", context.session.id),
            json!({"expected_session_revision":1,"input":user()}),
        )
        .await;
    let next_run = app.claim(&app.client, uuid(&next["run"]["id"])).await;
    assert!(
        next_run
            .context(&ContextQuery::default())
            .await
            .unwrap()
            .checkpoint
            .is_none()
    );
    let state = next_run
        .session_state(&query("codex.exec", Some("12")))
        .await
        .unwrap();
    assert_eq!(state.session_id, context.session.id);
    assert_eq!(state.items[0].saved_by_run_id, run.lease().run_id);
    let mut c = base(&next_run).await;
    c.session_state
        .push(set("codex.exec", "12", 1, json!({"cursor":9})));
    let reply = next_run.commit(&command(c)).await.unwrap();
    assert_eq!(
        reply.session_revision, 1,
        "private state must not advance transcript revision"
    );
    assert_eq!(reply.session_state[0].version, 2);
    conflict(
        run.session_state(&query("codex.exec", None)).await,
        ConflictCode::LeaseLost,
    );

    // Private data is absent from public session/history/events and ordinary context.
    for path in [
        format!("/api/sessions/{}", context.session.id),
        format!("/api/sessions/{}/messages", context.session.id),
        format!("/api/runs/{}/events", run.lease().run_id),
    ] {
        let text = reqwest::get(format!("{}{path}", app.url))
            .await
            .unwrap()
            .text()
            .await
            .unwrap();
        assert!(!text.contains("durable-handle"));
    }
}

#[sqlx::test(migrations = "./migrations")]
#[ignore = "requires DATABASE_URL; isolated SQLx database"]
async fn state_conflicts_roll_back_the_entire_commit_and_retries_are_idempotent(pool: PgPool) {
    let app = App::new(pool).await;
    let run = app.start().await;
    let mut c = base(&run).await;
    c.session_state.push(set("tool", "a", 0, json!({"v":1})));
    let original = command(c);
    let saved = run.commit(&original).await.unwrap();
    let pending = run.inputs(&SequenceQuery::default()).await.unwrap().items[0].id;
    let mut c = base(&run).await;
    c.checkpoint = Some(Checkpoint {
        expected_version: 0,
        state: Default::default(),
    });
    c.input_results.push(InputResult {
        id: pending,
        status: InputStatus::Handled,
        handling: Default::default(),
    });
    c.messages.push(AppendMessage {
        message_id: Uuid::now_v7(),
        message: assistant(),
    });
    c.session_state = vec![
        set("tool", "b", 0, json!({"v":2})),
        set("tool", "a", 0, json!({"v":3})),
    ];
    conflict(
        run.commit(&command(c)).await,
        ConflictCode::SessionStateVersionConflict,
    );
    let ctx = run.context(&ContextQuery::default()).await.unwrap();
    assert_eq!(ctx.run.run.version, saved.run.run.version);
    assert!(ctx.checkpoint.is_none() && ctx.messages.items.is_empty());
    assert_eq!(
        run.inputs(&SequenceQuery::default()).await.unwrap().items[0].id,
        pending
    );
    assert!(
        run.session_state(&query("tool", Some("b")))
            .await
            .unwrap()
            .items
            .is_empty()
    );

    // Fail after state application as well: neither state nor checkpoint may leak.
    let mut c = base(&run).await;
    c.session_state.push(set("tool", "b", 0, json!({"v":4})));
    c.cancel_wait_ids.push(Uuid::new_v4());
    conflict(
        run.commit(&command(c)).await,
        ConflictCode::WaitStateConflict,
    );
    assert!(
        run.session_state(&query("tool", Some("b")))
            .await
            .unwrap()
            .items
            .is_empty()
    );
    let mut c = base(&run).await;
    c.session_state.push(set("tool", "a", 1, json!({"v":5})));
    run.commit(&command(c)).await.unwrap();
    let replay = run.commit(&original).await.unwrap();
    assert_eq!(
        replay.session_state[0].version, 1,
        "receipt stays historical"
    );
    assert_eq!(
        run.session_state(&query("tool", Some("a")))
            .await
            .unwrap()
            .items[0]
            .version,
        2
    );
    let mut changed: Value = serde_json::to_value(original.body()).unwrap();
    changed["session_state"][0]["mutation"]["value"] = json!({"different":true});
    conflict(
        run.commit(&Command::new(
            original.key().clone(),
            serde_json::from_value(changed).unwrap(),
        ))
        .await,
        ConflictCode::IdempotencyKeyConflict,
    );
}

#[sqlx::test(migrations = "./migrations")]
#[ignore = "requires DATABASE_URL; isolated SQLx database"]
async fn tombstones_preserve_versions_and_namespace_pagination_is_bounded(pool: PgPool) {
    let app = App::new(pool).await;
    let run = app.start().await;
    let mut c = base(&run).await;
    c.session_state = vec![
        set("one", "a", 0, json!({})),
        set("one", "b", 0, json!({})),
        set("one", "c", 0, json!({})),
        set("two", "a", 0, json!({"separate":true})),
    ];
    run.commit(&command(c)).await.unwrap();
    let mut c = base(&run).await;
    c.session_state.push(SessionStateWrite {
        namespace: "one".into(),
        key: "b".into(),
        expected_version: 1,
        mutation: SessionStateMutation::Delete {},
    });
    let deleted = run.commit(&command(c)).await.unwrap();
    assert!(deleted.session_state[0].value.is_none());
    assert_eq!(deleted.session_state[0].version, 2);
    let page = run
        .session_state(&SessionStateQuery {
            limit: Some(2),
            ..query("one", None)
        })
        .await
        .unwrap();
    assert_eq!(
        page.items
            .iter()
            .map(|i| i.key.as_str())
            .collect::<Vec<_>>(),
        vec!["a", "b"]
    );
    assert!(page.items[1].value.is_none());
    let page = run
        .session_state(&SessionStateQuery {
            after_key: page.next_after_key,
            limit: Some(2),
            ..query("one", None)
        })
        .await
        .unwrap();
    assert_eq!(page.items[0].key, "c");
    assert!(page.next_after_key.is_none());
    for stale in [0, 1] {
        let mut c = base(&run).await;
        c.session_state
            .push(set("one", "b", stale, json!({"stale":true})));
        conflict(
            run.commit(&command(c)).await,
            ConflictCode::SessionStateVersionConflict,
        );
    }
    let mut c = base(&run).await;
    c.session_state
        .push(set("one", "b", 2, json!({"recreated":true})));
    assert_eq!(
        run.commit(&command(c)).await.unwrap().session_state[0].version,
        3
    );
    assert_eq!(
        run.session_state(&query("two", Some("a")))
            .await
            .unwrap()
            .items[0]
            .version,
        1
    );
}

#[sqlx::test(migrations = "./migrations")]
#[ignore = "requires DATABASE_URL; isolated SQLx database"]
async fn takeover_recovers_state_and_rejects_stale_or_unrelated_workers(pool: PgPool) {
    let app = App::new(pool).await;
    let run = app.start().await;
    let mut c = base(&run).await;
    c.session_state
        .push(set("tool", "saved", 0, json!({"handle":"one"})));
    let cmd = command(c);
    run.commit(&cmd).await.unwrap();
    let second = App::worker(&app.url).await;
    let unrelated = second.run(run.lease()).unwrap();
    conflict(
        unrelated.session_state(&query("tool", None)).await,
        ConflictCode::LeaseLost,
    );
    conflict(unrelated.commit(&cmd).await, ConflictCode::LeaseLost);
    sqlx::query(
        "update runs set lease_expires_at=clock_timestamp()-interval '1 second' where id=$1",
    )
    .bind(run.lease().run_id)
    .execute(&app.pool)
    .await
    .unwrap();
    let replacement = app.claim(&second, run.lease().run_id).await;
    let state = replacement
        .session_state(&query("tool", Some("saved")))
        .await
        .unwrap();
    assert_eq!(state.items[0].value.as_ref().unwrap()["handle"], "one");
    replacement.commit(&cmd).await.unwrap(); // Stable receipt, no repeated write.
    let mut c = base(&replacement).await;
    c.session_state
        .push(set("tool", "saved", 1, json!({"handle":"two"})));
    let update = command(c);
    conflict(run.commit(&update).await, ConflictCode::LeaseLost);
    conflict(
        run.session_state(&query("tool", None)).await,
        ConflictCode::LeaseLost,
    );
    let saved = replacement.commit(&update).await.unwrap();
    assert_eq!(
        saved.session_state[0].saved_by_lease_epoch,
        replacement.lease().lease_epoch
    );
    assert_eq!(saved.session_state[0].version, 2);
}

#[sqlx::test(migrations = "./migrations")]
#[ignore = "requires DATABASE_URL; isolated SQLx database"]
async fn concurrent_storage_commits_accept_only_one_writer(pool: PgPool) {
    let app = App::new(pool).await;
    let run = app.start().await;
    let mut a = base(&run).await;
    let mut b = base(&run).await;
    a.session_state = vec![
        set("n", "a", 0, json!({"writer":"a"})),
        set("n", "b", 0, json!({})),
    ];
    b.session_state = vec![
        set("n", "b", 0, json!({"writer":"b"})),
        set("n", "a", 0, json!({})),
    ];
    let (a, b) = (command(a), command(b));
    let (ra, rb) = tokio::join!(run.commit(&a), run.commit(&b));
    assert_eq!(usize::from(ra.is_ok()) + usize::from(rb.is_ok()), 1);
    conflict(
        if ra.is_err() { ra } else { rb },
        ConflictCode::RunVersionConflict,
    );
    let entries = run.session_state(&query("n", None)).await.unwrap().items;
    assert_eq!(entries.len(), 2);
    assert!(entries.iter().all(|e| e.version == 1));
}

#[sqlx::test(migrations = "./migrations")]
#[ignore = "requires DATABASE_URL; isolated SQLx database"]
async fn parking_commits_tool_state_checkpoint_and_input_handling_together(pool: PgPool) {
    let app = App::new(pool).await;
    let run = app.start().await;
    let mut c = base(&run).await;
    c.session_state.push(set(
        "tool",
        "pending",
        0,
        json!({"operation":"saved-before-parking"}),
    ));
    c.checkpoint = Some(Checkpoint {
        expected_version: 0,
        state: json!({"phase":"approval"}).as_object().unwrap().clone(),
    });
    c.input_results = run
        .inputs(&SequenceQuery::default())
        .await
        .unwrap()
        .items
        .into_iter()
        .map(|i| InputResult {
            id: i.id,
            status: InputStatus::Handled,
            handling: Default::default(),
        })
        .collect();
    c.waits.push(Wait {
        wait_key: "approve".into(),
        mode: WaitMode::Any,
        deadline_at: None,
        metadata: Default::default(),
        dependencies: vec![Dependency::Input {
            input_kind: "approval_response".into(),
            correlation_key: Some("approve-op".into()),
        }],
    });
    c.disposition = Disposition::Waiting;
    let parked = run.commit(&command(c)).await.unwrap();
    assert_eq!(parked.run.run.status, RunStatus::Waiting);
    assert!(parked.run.worker_id.is_none());
    assert_eq!(parked.session_state[0].version, 1);
    conflict(
        run.session_state(&query("tool", None)).await,
        ConflictCode::LeaseLost,
    );
    app.post(
        &format!("/api/runs/{}/inputs", run.lease().run_id),
        json!({"kind":"approval_response","correlation_key":"approve-op","approved":true}),
    )
    .await;
    let next_worker = App::worker(&app.url).await;
    let resumed = app.claim(&next_worker, run.lease().run_id).await;
    let ctx = resumed.context(&ContextQuery::default()).await.unwrap();
    assert_eq!(ctx.checkpoint.unwrap().state["phase"], "approval");
    assert_eq!(ctx.waits.items[0].status, WaitStatus::Satisfied);
    let state = resumed
        .session_state(&query("tool", Some("pending")))
        .await
        .unwrap();
    assert_eq!(
        state.items[0].value.as_ref().unwrap()["operation"],
        "saved-before-parking"
    );
    assert_eq!(
        resumed
            .inputs(&SequenceQuery::default())
            .await
            .unwrap()
            .items
            .len(),
        1,
        "the original input was acknowledged in the parking commit"
    );
}

#[sqlx::test(migrations = "./migrations")]
#[ignore = "requires DATABASE_URL; isolated SQLx database"]
async fn validation_authentication_and_private_scope_fail_closed(pool: PgPool) {
    let app = App::new(pool).await;
    let run = app.start().await;
    for writes in [
        vec![set("Bad", "x", 0, json!({}))],
        vec![set("ok", "bad key", 0, json!({}))],
        vec![set("ok", "x", -1, json!({}))],
        vec![set("ok", "x", i64::MAX, json!({}))],
        vec![set("ok", "x", 0, json!({})), set("ok", "x", 0, json!({}))],
        vec![set(
            "ok",
            "x",
            0,
            json!({"large":"x".repeat(SESSION_STATE_MAX_VALUE_BYTES)}),
        )],
        (0..201)
            .map(|i| set("ok", &i.to_string(), 0, json!({})))
            .collect(),
    ] {
        let mut c = base(&run).await;
        c.session_state = writes;
        assert!(matches!(run.commit(&command(c)).await,Err(Error::Server(e)) if e.status==400));
    }
    let path = format!(
        "{}/internal/runs/{}/session-state",
        app.url,
        run.lease().run_id
    );
    for token in ["", BOOT, "wrong"] {
        let reply = reqwest::Client::new()
            .get(&path)
            .bearer_auth(token)
            .header("x-worker-id", app.client.worker_id().to_string())
            .header("x-lease-epoch", run.lease().lease_epoch)
            .query(&query("ok", None))
            .send()
            .await
            .unwrap();
        assert_eq!(reply.status(), 401);
    }
    for params in [
        "namespace=ok&session_id=forged",
        "namespace=ok&limit=51",
        "namespace=ok&key=a&after_key=b",
        "namespace=Bad",
    ] {
        let reply = reqwest::Client::new()
            .get(format!("{path}?{params}"))
            .bearer_auth(TOKEN)
            .header("x-worker-id", app.client.worker_id().to_string())
            .header("x-lease-epoch", run.lease().lease_epoch)
            .send()
            .await
            .unwrap();
        assert_eq!(reply.status(), 400);
    }
    assert!(
        run.session_state(&query("ok", None))
            .await
            .unwrap()
            .items
            .is_empty()
    );
}
