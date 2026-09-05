//! SDK contract tests against the real HTTP routes and isolated PostgreSQL.
use platform_runtime_client::{
    ClientConfig, Command, Error, PlatformClient, RequestKey, RunClient, WorkerRegistration,
    types::*,
};
use platform_server::runtime::{RuntimeService, router, worker_router};
use serde_json::{Value, json};
use sqlx::PgPool;
use uuid::Uuid;

const BOOT: &str = "bootstrap-sdk-test-012345678901234567890";
const TOKEN: &str = "worker-sdk-test-012345678901234567890123";
struct App {
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
        Self::with_environment_gateway(pool, None).await
    }
    async fn with_environment_gateway(
        pool: PgPool,
        gateway: Option<platform_server::projects::environments::EnvironmentGateway>,
    ) -> Self {
        let project = Uuid::now_v7();
        sqlx::query("insert into projects(project_id,name) values($1,'SDK tests')")
            .bind(project)
            .execute(&pool)
            .await
            .unwrap();
        sqlx::query("insert into harnesses(id,name) values('test','Test')")
            .execute(&pool)
            .await
            .unwrap();
        let mut service = RuntimeService::new(pool.clone());
        if let Some(gateway) = gateway {
            service = service.with_environments(
                platform_server::projects::environments::EnvironmentService::new(pool, gateway),
            );
        }
        let app = router(service.clone()).merge(worker_router(service, BOOT));
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("http://{}", listener.local_addr().unwrap());
        let task = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        let client =
            PlatformClient::new(&url, Uuid::new_v4(), TOKEN, ClientConfig::default()).unwrap();
        client.register(BOOT, &registration()).await.unwrap();
        Self {
            url,
            project,
            client,
            task,
        }
    }
    async fn create(&self) -> ChildResponse {
        // Public creation is deliberately not part of the worker's mutation SDK.
        reqwest::Client::new().post(format!("{}/api/projects/{}/sessions",self.url,self.project))
            .header("Idempotency-Key",Uuid::new_v4().to_string())
            .json(&json!({"harness_id":"test","title":"SDK session","initial_run":{"expected_session_revision":0,"input":user()}}))
            .send().await.unwrap().error_for_status().unwrap().json().await.unwrap()
    }
    async fn start(&self) -> (ChildResponse, RunClient, RunContext) {
        let created = self.create().await;
        let claimed = self
            .client
            .claim(&command("initial-claim", Claim { limit: 1 }))
            .await
            .unwrap();
        let run = self.client.run(claimed.items[0].lease()).unwrap();
        let ctx = run.context(&ContextQuery::default()).await.unwrap();
        (created, run, ctx)
    }
}
fn registration() -> WorkerRegistration {
    WorkerRegistration {
        build_id: "sdk-v1".into(),
        supported_harnesses: vec!["test".into()],
        capacity: 2,
    }
}
fn command<T>(key: &str, body: T) -> Command<T> {
    Command::new(RequestKey::new(key).unwrap(), body)
}
fn user() -> Value {
    json!({"role":"user","id":"native-user-id","timestamp":0,"content":[{"type":"text","content":"Work"}]})
}
fn assistant() -> Message {
    serde_json::from_value(json!({"role":"assistant","id":"provider-response-id","timestamp":0,"model":{"provider":"test","id":"model"},"duration_ms":1,"native_message":{},"content":[{"type":"response","response":{"content":"Done"}}],"stop_reason":"stop"})).unwrap()
}
fn base(ctx: &RunContext) -> Commit {
    Commit::new(ctx.run.run.version, ctx.session.current_revision)
}

#[sqlx::test(migrations = "./migrations")]
#[ignore = "requires DATABASE_URL with CREATEDB"]
async fn sdk_environment_creation_is_project_scoped_fenced_and_replayable(pool: PgPool) {
    use axum::{Json, Router, extract::Path, routing::get};
    use std::{
        sync::{
            Arc,
            atomic::{AtomicBool, Ordering},
        },
        time::Duration,
    };
    let available = Arc::new(AtomicBool::new(true));
    let expire_run = Arc::new(tokio::sync::Mutex::new(None::<Uuid>));
    let flag = available.clone();
    let expires = expire_run.clone();
    let db = pool.clone();
    let gateway_app = Router::new().route("/v1/hosts/{id}", get(move |Path(id): Path<Uuid>| {
        let flag = flag.clone(); let expires = expires.clone(); let db = db.clone();
        async move {
            if let Some(run) = *expires.lock().await {
                sqlx::query("update runs set lease_expires_at=clock_timestamp()-interval '1 second' where id=$1").bind(run).execute(&db).await.unwrap();
            }
            if flag.load(Ordering::SeqCst) {
                (axum::http::StatusCode::OK, Json(json!({"id":id,"kind":"registered","desired_state":"ready","deleted_at":null,"descriptor":{"roots":[{"id":"workspace","native_path":"/home/user"}]}})))
            } else { (axum::http::StatusCode::NOT_FOUND, Json(json!({}))) }
        }
    }));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let gateway = platform_server::projects::environments::EnvironmentGateway::new(
        format!("http://{}", listener.local_addr().unwrap())
            .parse()
            .unwrap(),
        "test-token",
        Duration::from_secs(3),
    )
    .unwrap();
    let gateway_task = tokio::spawn(async move {
        axum::serve(listener, gateway_app).await.unwrap();
    });
    let app = App::with_environment_gateway(pool.clone(), Some(gateway)).await;
    let (created, run, _) = app.start().await;
    let body = CreateEnvironment {
        name: "Test environment".into(),
        kind: EnvironmentType::Machine,
        machine_id: Some(Uuid::new_v4()),
        snapshot_id: None,
        workspace_root: "/home/user".into(),
        path: "test".into(),
    };
    let cmd = command("environment-create", body.clone());
    let (a, b) = tokio::join!(run.create_environment(&cmd), run.create_environment(&cmd));
    let saved = a.unwrap();
    assert_eq!(saved.id, b.unwrap().id);
    assert_eq!(saved.project_id, app.project);
    assert_eq!(saved.workspace_root, "/home/user");
    let mut changed = body.clone();
    changed.path = "other".into();
    conflict(
        run.create_environment(&command("environment-create", changed))
            .await,
        ConflictCode::IdempotencyKeyConflict,
    );
    let other_project = Uuid::new_v4();
    sqlx::query("insert into projects(project_id,name) values($1,'Other')")
        .bind(other_project)
        .execute(&pool)
        .await
        .unwrap();
    sqlx::query("insert into project_environments(id,project_id,name,type,machine_id,workspace_root,path) values($1,$2,'Hidden','machine',$3,'/workspace','.')").bind(Uuid::new_v4()).bind(other_project).bind(body.machine_id).execute(&pool).await.unwrap();
    let listed = run.environments().await.unwrap();
    assert_eq!(listed.items.len(), 1);
    assert_eq!(listed.items[0].id, saved.id);

    // Lease expiry during the external validation must roll back the new write.
    *expire_run.lock().await = Some(created.run.id);
    conflict(
        run.create_environment(&command("expires-during-validation", body.clone()))
            .await,
        ConflictCode::LeaseLost,
    );
    *expire_run.lock().await = None;
    conflict(run.environments().await, ConflictCode::LeaseLost);
    available.store(false, Ordering::SeqCst);
    let replacement = PlatformClient::new(
        &app.url,
        Uuid::new_v4(),
        "replacement-token-01234567890123456789",
        ClientConfig::default(),
    )
    .unwrap();
    replacement.register(BOOT, &registration()).await.unwrap();
    let claim = replacement
        .claim(&command("takeover-environment", Claim { limit: 1 }))
        .await
        .unwrap();
    let recovered = replacement.run(claim.items[0].lease()).unwrap();
    // Neither a vanished reference nor a new worker invalidates the receipt.
    assert_eq!(
        recovered.create_environment(&cmd).await.unwrap().id,
        saved.id
    );
    assert_eq!(run.create_environment(&cmd).await.unwrap().id, saved.id);
    conflict(
        run.create_environment(&command("stale-environment", body))
            .await,
        ConflictCode::LeaseLost,
    );
    let count: i64 =
        sqlx::query_scalar("select count(*) from project_environments where project_id=$1")
            .bind(app.project)
            .fetch_one(&pool)
            .await
            .unwrap();
    assert_eq!(count, 1);
    gateway_task.abort();
}
fn conflict<T>(result: Result<T, Error>, expected: ConflictCode) {
    assert_eq!(
        result.err().expect("expected conflict").conflict(),
        Some(expected)
    );
}

#[sqlx::test(migrations = "./migrations")]
#[ignore = "requires DATABASE_URL with CREATEDB"]
async fn sdk_follow_up_preserves_child_history_and_accepts_further_messages(pool: PgPool) {
    let app = App::new(pool).await;
    let (created, parent, _) = app.start().await;
    let child = parent
        .create_child(&command(
            "child",
            Child {
                config_override: Default::default(),
                harness_id: None,
                title: None,
                fork_at_revision: None,
                initial_run: StartRun {
                    input: serde_json::from_value(user()).unwrap(),
                    expected_session_revision: 0,
                },
            },
        ))
        .await
        .unwrap();
    let claim = app
        .client
        .claim(&command("claim-child", Claim { limit: 1 }))
        .await
        .unwrap();
    let child_run = app.client.run(claim.items[0].lease()).unwrap();
    let ctx = child_run.context(&ContextQuery::default()).await.unwrap();
    let mut finish = base(&ctx);
    let final_id = Uuid::now_v7();
    finish.messages.push(AppendMessage {
        message_id: final_id,
        message: assistant(),
    });
    finish.input_results.push(InputResult {
        id: child.input.id,
        status: InputStatus::Handled,
        handling: Default::default(),
    });
    finish.disposition = Disposition::Completed {
        final_message_id: final_id,
    };
    child_run
        .commit(&command("finish-child", finish))
        .await
        .unwrap();
    // A parent needs only its RunClient, exactly as supplied in Execution.
    // No separately injected worker-management client or child lease is needed.
    let reads = parent.queries();
    let finished = reads.run_state(child.run.id).await.unwrap();
    assert_eq!(finished.status, RunStatus::Completed);
    assert_eq!(finished.final_message_id, Some(final_id));
    let children = reads
        .children(created.run.id, &ListQuery::default())
        .await
        .unwrap();
    assert_eq!(children.items.len(), 1);
    assert_eq!(children.items[0].id, child.run.id);
    let answer = reads
        .session_messages(finished.session_id, &MessageQuery::default())
        .await
        .unwrap();
    assert_eq!(answer.items.len(), 1);
    assert_eq!(answer.items[0].message_id, final_id);
    assert_eq!(
        serde_json::to_value(&answer.items[0].message).unwrap(),
        serde_json::to_value(assistant()).unwrap()
    );
    let follow_up = command(
        "follow-up",
        FollowUp {
            target_session_id: child.session.id,
            run: StartRun {
                input: serde_json::from_value(user()).unwrap(),
                expected_session_revision: 1,
            },
        },
    );
    // Commands remain serializable for checkpoint/recovery before dispatch.
    let recovered: Command<FollowUp> =
        serde_json::from_value(serde_json::to_value(&follow_up).unwrap()).unwrap();
    let next = parent.follow_up(&recovered).await.unwrap();
    assert_eq!(
        parent.follow_up(&recovered).await.unwrap().run.id,
        next.run.id
    );
    assert_eq!(next.session.id, child.session.id);
    assert_eq!(next.session.current_revision, 1);
    assert_eq!(next.run.parent_run_id, Some(created.run.id));
    assert_ne!(next.run.id, child.run.id);
    let history = reads
        .session_messages(child.session.id, &MessageQuery::default())
        .await
        .unwrap();
    assert_eq!(history.items.len(), 1);
    assert_eq!(history.items[0].run_id, Some(child.run.id));
    let sent = parent
        .send_message(&command(
            "further-message",
            RunMessage {
                target_run_id: next.run.id,
                kind: "agent_message".into(),
                payload: json!({"text":"Also check the edge cases"})
                    .as_object()
                    .unwrap()
                    .clone(),
            },
        ))
        .await
        .unwrap();
    assert_eq!(sent.input.source_run_id, Some(created.run.id));
    assert_eq!(sent.run.id, next.run.id);
    assert_eq!(
        reads
            .session_runs(child.session.id, &ListQuery::default())
            .await
            .unwrap()
            .items
            .len(),
        2
    );
}

#[sqlx::test(migrations = "./migrations")]
#[ignore = "requires DATABASE_URL with CREATEDB"]
async fn sdk_worker_recovery_coordination_and_history_round_trip(pool: PgPool) {
    let app = App::new(pool).await;
    let client = &app.client;
    let empty = command("empty-claim", Claim { limit: 1 });
    assert!(client.claim(&empty).await.unwrap().items.is_empty());
    let created = app.create().await;
    // An empty claim receipt is stable, not a reusable polling request.
    assert!(client.claim(&empty).await.unwrap().items.is_empty());
    let claimed = client
        .claim(&command("new-claim", Claim { limit: 1 }))
        .await
        .unwrap();
    assert_eq!(claimed.items[0].run.run.id, created.run.id);
    let lease = claimed.items[0].lease();
    let run = client.run(lease).unwrap();
    let assignments = client.assignments().await.unwrap();
    assert_eq!(assignments.items.len(), 1);
    let heartbeat = client
        .heartbeat(&Heartbeat {
            leases: vec![lease],
        })
        .await
        .unwrap();
    assert_eq!(heartbeat.renewed.len(), 1);
    // Migrations may seed built-in harnesses in addition to this test fixture.
    assert!(
        client
            .harnesses()
            .await
            .unwrap()
            .items
            .iter()
            .any(|h| h.id == "test")
    );
    assert_eq!(client.harness("test").await.unwrap().id, "test");
    assert_eq!(
        client
            .sessions(app.project, &ListQuery::default())
            .await
            .unwrap()
            .items
            .len(),
        1
    );
    let ctx = run.context(&ContextQuery::default()).await.unwrap();
    assert!(ctx.checkpoint.is_none());
    assert_eq!(ctx.run.run.version, heartbeat.renewed[0].version);
    let inputs = run.inputs(&SequenceQuery::default()).await.unwrap();
    assert_eq!(inputs.items[0].id, created.input.id);
    assert_eq!(
        run.inputs(&SequenceQuery::default()).await.unwrap().items[0].status,
        InputState::Pending
    );

    // Persist the exact child command as harness recovery state BEFORE sending.
    let child_command = command(
        "child-1",
        Child {
            config_override: Default::default(),
            harness_id: None,
            title: Some("Child".into()),
            fork_at_revision: Some(2),
            initial_run: StartRun {
                input: serde_json::from_value(user()).unwrap(),
                expected_session_revision: 2,
            },
        },
    );
    let mut first = base(&ctx);
    first.messages = (0..2)
        .map(|_| AppendMessage {
            message_id: Uuid::now_v7(),
            message: serde_json::from_value(user()).unwrap(),
        })
        .collect();
    first.input_results.push(InputResult {
        id: created.input.id,
        status: InputStatus::Handled,
        handling: Default::default(),
    });
    first.checkpoint = Some(Checkpoint {
        expected_version: 0,
        state: json!({"child_request":child_command})
            .as_object()
            .unwrap()
            .clone(),
    });
    let saved = run.commit(&command("checkpoint-1", first)).await.unwrap();
    assert_eq!(saved.session_revision, 2);
    assert_eq!(saved.checkpoint.unwrap().version, 1);
    let ctx = run
        .context(&ContextQuery {
            limit: Some(1),
            ..Default::default()
        })
        .await
        .unwrap();
    assert_eq!(ctx.messages.items.len(), 1);
    assert_eq!(ctx.messages.next_after_revision, Some(1));
    let page2 = run
        .context(&ContextQuery {
            after_revision: ctx.messages.next_after_revision,
            limit: Some(1),
            ..Default::default()
        })
        .await
        .unwrap();
    assert_eq!(page2.messages.items[0].revision, 2);
    assert!(page2.messages.next_after_revision.is_none());
    let recovered: Command<Child> =
        serde_json::from_value(ctx.checkpoint.unwrap().state["child_request"].clone()).unwrap();
    let child = run.create_child(&recovered).await.unwrap();
    assert_eq!(
        run.create_child(&recovered).await.unwrap().run.id,
        child.run.id
    );
    assert_eq!(child.run.parent_run_id, Some(created.run.id));
    assert_eq!(child.session.current_revision, 2);
    assert_eq!(
        client
            .children(created.run.id, &ListQuery::default())
            .await
            .unwrap()
            .items
            .len(),
        1
    );
    assert_eq!(
        client
            .session_runs(created.session.id, &ListQuery::default())
            .await
            .unwrap()
            .items
            .len(),
        1
    );
    assert_eq!(
        client
            .session(created.session.id)
            .await
            .unwrap()
            .current_revision,
        2
    );
    let history = client
        .session_messages(
            child.session.id,
            &MessageQuery {
                limit: Some(1),
                ..Default::default()
            },
        )
        .await
        .unwrap();
    assert!(history.items[0].run_id.is_none());
    assert_eq!(history.items[0].origin_run_id, Some(created.run.id));

    let context = run.context(&ContextQuery::default()).await.unwrap();
    let mut wait = base(&context);
    wait.waits = vec![Wait {
        wait_key: "join".into(),
        mode: WaitMode::All,
        deadline_at: None,
        metadata: Default::default(),
        dependencies: vec![Dependency::RunCompletion {
            target_run_id: child.run.id,
        }],
    }];
    let wait_id = run
        .commit(&command("register-wait", wait))
        .await
        .unwrap()
        .wait_ids[0];
    let waits = client
        .run_waits(created.run.id, &ListQuery::default())
        .await
        .unwrap();
    assert_eq!(
        waits.items[0].dependencies[0].target_run_id,
        Some(child.run.id)
    );
    assert_eq!(
        run.context(&ContextQuery::default())
            .await
            .unwrap()
            .waits
            .items[0]
            .id,
        wait_id
    );
    let message = run
        .send_message(&command(
            "message-1",
            RunMessage {
                target_run_id: child.run.id,
                kind: "operation_result".into(),
                payload: json!({"correlation_key":"op-1","value":42})
                    .as_object()
                    .unwrap()
                    .clone(),
            },
        ))
        .await
        .unwrap();
    assert_eq!(message.input.source_run_id, Some(created.run.id));
    let child_claim = client
        .claim(&command("child-claim", Claim { limit: 1 }))
        .await
        .unwrap();
    let child_lease = child_claim.items[0].lease();
    let child_run = client.run(child_lease).unwrap();
    let child_inputs = child_run
        .inputs(&SequenceQuery {
            limit: Some(1),
            ..Default::default()
        })
        .await
        .unwrap();
    assert_eq!(child_inputs.next_after_sequence, Some(1));
    let child_inputs2 = child_run
        .inputs(&SequenceQuery {
            limit: Some(1),
            after_sequence: child_inputs.next_after_sequence,
            ..Default::default()
        })
        .await
        .unwrap();
    assert_eq!(child_inputs2.items[0].id, message.input.id);
    assert_eq!(
        client
            .run_inputs(child.run.id, &SequenceQuery::default())
            .await
            .unwrap()
            .items
            .len(),
        2
    );
    let abort = command(
        "abort-child",
        AbortRequest {
            target_run_id: child.run.id,
            reason: Some("Enough".into()),
        },
    );
    assert!(run.request_abort(&abort).await.unwrap().input.is_some());
    let child_ctx = child_run.context(&ContextQuery::default()).await.unwrap();
    let mut stop = base(&child_ctx);
    stop.disposition = Disposition::Aborted;
    child_run.commit(&command("abort-ack", stop)).await.unwrap();
    let terminal_abort = run
        .request_abort(&command(
            "terminal-cleanup",
            AbortRequest {
                target_run_id: child.run.id,
                reason: None,
            },
        ))
        .await
        .unwrap();
    assert!(terminal_abort.input.is_none());
    assert_eq!(terminal_abort.run.status, RunStatus::Aborted);

    let events = command(
        "events",
        Events {
            events: vec![HarnessEvent {
                r#type: "model.delta".into(),
                payload: Default::default(),
                occurred_at: None,
            }],
        },
    );
    assert_eq!(
        run.append_events(&events).await.unwrap().items[0].r#type,
        "model.delta"
    );
    let ctx = run.context(&ContextQuery::default()).await.unwrap();
    let mut finish = base(&ctx);
    finish.cancel_wait_ids.push(wait_id);
    let message_id = Uuid::now_v7();
    finish.messages.push(AppendMessage {
        message_id,
        message: assistant(),
    });
    finish.disposition = Disposition::Completed {
        final_message_id: message_id,
    };
    let finish = command("complete", finish);
    let result = run.commit(&finish).await.unwrap();
    assert_eq!(result.run.run.status, RunStatus::Completed);
    // Successful terminal receipts remain accessible with the historical epoch.
    let replay = run.commit(&finish).await.unwrap();
    assert_eq!(replay.run.run.version, result.run.run.version);
    assert_eq!(
        client.run_state(created.run.id).await.unwrap().status,
        RunStatus::Completed
    );
    conflict(
        run.context(&ContextQuery::default()).await,
        ConflictCode::LeaseLost,
    );
    let heartbeat = client
        .heartbeat(&Heartbeat {
            leases: vec![lease, child_lease],
        })
        .await
        .unwrap();
    assert_eq!(heartbeat.lost.len(), 2);
    assert!(heartbeat.renewed.is_empty());
    let events = client
        .run_events(
            created.run.id,
            &SequenceQuery {
                limit: Some(1),
                ..Default::default()
            },
        )
        .await
        .unwrap();
    assert_eq!(events.items.len(), 1);
    assert!(events.next_after_sequence.is_some());
    assert!(client.assignments().await.unwrap().items.is_empty());
    client
        .patch_worker(&PatchWorker {
            status: Some(WorkerStatus::Draining),
            capacity: Some(1),
        })
        .await
        .unwrap();
    // Re-registration returns metadata without undoing drain/capacity changes.
    assert_eq!(
        client
            .register(BOOT, &registration())
            .await
            .unwrap()
            .capacity,
        1
    );
    client
        .patch_worker(&PatchWorker {
            status: Some(WorkerStatus::Offline),
            capacity: None,
        })
        .await
        .unwrap();
    conflict(
        client.heartbeat(&Heartbeat { leases: vec![] }).await,
        ConflictCode::WorkerStateConflict,
    );
}

#[sqlx::test(migrations = "./migrations")]
#[ignore = "requires DATABASE_URL with CREATEDB"]
async fn sdk_conflicts_are_distinct_and_receipts_survive_takeover(pool: PgPool) {
    let app = App::new(pool.clone()).await;
    let (created, run, ctx) = app.start().await;
    let mut c = base(&ctx);
    c.expected_run_version += 1;
    conflict(
        run.commit(&command("wrong-run-version", c)).await,
        ConflictCode::RunVersionConflict,
    );
    let mut c = base(&ctx);
    c.expected_session_revision += 1;
    conflict(
        run.commit(&command("wrong-session-version", c)).await,
        ConflictCode::SessionRevisionConflict,
    );
    let mut c = base(&ctx);
    c.checkpoint = Some(Checkpoint {
        expected_version: 7,
        state: Default::default(),
    });
    conflict(
        run.commit(&command("wrong-checkpoint-version", c)).await,
        ConflictCode::CheckpointVersionConflict,
    );
    let mut c = base(&ctx);
    c.input_results.push(InputResult {
        id: Uuid::new_v4(),
        status: InputStatus::Handled,
        handling: Default::default(),
    });
    conflict(
        run.commit(&command("wrong-input", c)).await,
        ConflictCode::InputStateConflict,
    );
    let mut c = base(&ctx);
    c.cancel_wait_ids.push(Uuid::new_v4());
    conflict(
        run.commit(&command("wrong-wait", c)).await,
        ConflictCode::WaitStateConflict,
    );
    let mut c = base(&ctx);
    c.disposition = Disposition::Aborted;
    conflict(
        run.commit(&command("no-abort-request", c)).await,
        ConflictCode::RunStateConflict,
    );
    let first = command("first-commit", base(&ctx));
    let saved = run.commit(&first).await.unwrap();
    let mut changed = base(&ctx);
    changed.expected_run_version += 1;
    conflict(
        run.commit(&command("first-commit", changed)).await,
        ConflictCode::IdempotencyKeyConflict,
    );
    conflict(
        app.client
            .claim(&command("initial-claim", Claim { limit: 2 }))
            .await,
        ConflictCode::IdempotencyKeyConflict,
    );
    let impersonator = PlatformClient::new(
        &app.url,
        app.client.worker_id(),
        "different-token-012345678901234567890",
        ClientConfig::default(),
    )
    .unwrap();
    conflict(
        impersonator.register(BOOT, &registration()).await,
        ConflictCode::WorkerIdentityConflict,
    );

    sqlx::query(
        "update runs set lease_expires_at=clock_timestamp()-interval '1 second' where id=$1",
    )
    .bind(created.run.id)
    .execute(&pool)
    .await
    .unwrap();
    let replacement = PlatformClient::new(
        &app.url,
        Uuid::new_v4(),
        "replacement-token-01234567890123456789",
        ClientConfig::default(),
    )
    .unwrap();
    replacement.register(BOOT, &registration()).await.unwrap();
    let claim = replacement
        .claim(&command("takeover", Claim { limit: 1 }))
        .await
        .unwrap();
    let replacement_run = replacement.run(claim.items[0].lease()).unwrap();
    assert!(replacement_run.lease().lease_epoch > run.lease().lease_epoch);
    assert_eq!(
        replacement_run
            .commit(&first)
            .await
            .unwrap()
            .run
            .run
            .version,
        saved.run.run.version
    );
    assert_eq!(
        run.commit(&first).await.unwrap().run.run.version,
        saved.run.run.version
    );
    conflict(
        run.commit(&command("stale-new-write", base(&ctx))).await,
        ConflictCode::LeaseLost,
    );
    // Replayed commits don't overwrite newer ownership/version observations.
    let fresh = replacement_run
        .context(&ContextQuery::default())
        .await
        .unwrap();
    assert!(fresh.run.run.version > saved.run.run.version);
}
