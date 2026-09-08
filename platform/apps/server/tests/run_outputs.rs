//! Public output publication/recovery through real HTTP and isolated PostgreSQL.
use platform_runtime_client::{
    ClientConfig, Command, Error, PlatformClient, RequestKey, RunClient, WorkerRegistration,
    types::*,
};
use platform_server::runtime::{RuntimeService, admin_router, router, worker_router};
use serde_json::{Value, json};
use sqlx::PgPool;
use uuid::Uuid;

const BOOT: &str = "outputs-bootstrap-012345678901234567890";
const TOKEN: &str = "outputs-worker-012345678901234567890123";
const ADMIN: &str = "outputs-admin-0123456789012345678901234";

struct App {
    pool: PgPool,
    url: String,
    project: Uuid,
    other: Uuid,
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
        let other = Uuid::now_v7();
        for id in [project, other] {
            sqlx::query("insert into projects(project_id,name) values($1,'Output tests')")
                .bind(id)
                .execute(&pool)
                .await
                .unwrap();
        }
        sqlx::query("insert into harnesses(id,name) values('output-test','Outputs')")
            .execute(&pool)
            .await
            .unwrap();
        let runtime = RuntimeService::new(pool.clone());
        let routes = router(runtime.clone())
            .merge(worker_router(runtime.clone(), BOOT))
            .merge(admin_router(runtime, ADMIN));
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("http://{}", listener.local_addr().unwrap());
        let task = tokio::spawn(async move {
            axum::serve(listener, routes).await.unwrap();
        });
        let client = Self::worker(&url).await;
        let app = Self {
            pool,
            url,
            project,
            other,
            client,
            task,
        };
        app.http(
            "PUT",
            "/internal/harnesses/output-test",
            json!({"name":"Outputs","project_policy":"required","harness_contract":declarations()}),
            200,
        )
        .await;
        app
    }
    async fn worker(url: &str) -> PlatformClient {
        let client =
            PlatformClient::new(url, Uuid::new_v4(), TOKEN, ClientConfig::default()).unwrap();
        client
            .register(
                BOOT,
                &WorkerRegistration {
                    build_id: "test".into(),
                    supported_harnesses: vec!["output-test".into()],
                    capacity: 16,
                },
            )
            .await
            .unwrap();
        client
    }
    async fn http(&self, method: &str, path: &str, body: Value, expected: u16) -> Value {
        let response = reqwest::Client::new()
            .request(method.parse().unwrap(), format!("{}{path}", self.url))
            .bearer_auth(ADMIN)
            .header("idempotency-key", Uuid::new_v4().to_string())
            .json(&body)
            .send()
            .await
            .unwrap();
        let status = response.status().as_u16();
        let value: Value = response.json().await.unwrap();
        assert_eq!(status, expected, "{value}");
        value
    }
    async fn start_in(&self, project: Uuid, config: Value) -> RunClient {
        let result = self.http("POST", &format!("/api/projects/{project}/sessions"),
            json!({"harness_id":"output-test","config_override":config,"initial_run":{"expected_session_revision":0,"input":user()}}), 201).await;
        self.claim(&self.client, id(&result["run"]["id"])).await
    }
    async fn start(&self) -> RunClient {
        self.start_in(self.project, json!({})).await
    }
    async fn claim(&self, worker: &PlatformClient, run: Uuid) -> RunClient {
        let assigned = worker.claim(&command(Claim { limit: 16 })).await.unwrap();
        worker
            .run(
                assigned
                    .items
                    .iter()
                    .find(|a| a.run.run.id == run)
                    .unwrap()
                    .lease(),
            )
            .unwrap()
    }
    async fn environment(&self, project: Uuid) -> Uuid {
        let id = Uuid::now_v7();
        sqlx::query("insert into project_environments(id,project_id,name,type,machine_id,workspace_root,path) values($1,$2,'Work','machine',$3,'/work','.')")
            .bind(id).bind(project).bind(Uuid::now_v7()).execute(&self.pool).await.unwrap();
        id
    }
}
fn id(v: &Value) -> Uuid {
    serde_json::from_value(v.clone()).unwrap()
}
fn command<T>(body: T) -> Command<T> {
    Command::new(RequestKey::new(Uuid::new_v4().to_string()).unwrap(), body)
}
fn user() -> Value {
    json!({"role":"user","id":"input","timestamp":0,"content":[{"type":"text","content":"Work"}]})
}
fn declarations() -> Value {
    json!({"outputs":{
        "primary":{"kind":"execution_workspace"}, "secondary":{"kind":"execution_workspace"},
        "report":{"kind":"json","value_schema":{"type":"object","required":["score"],"properties":{"score":{"type":"number"}},"additionalProperties":false}},
        "artifact":{"kind":"artifact"}, "a":{"kind":"json"},"b":{"kind":"json"},"c":{"kind":"json"},"d":{"kind":"json"}
    }})
}
fn output(name: &str, value: Value) -> PublishRunOutput {
    PublishRunOutput {
        name: name.into(),
        output: OutputValue::Json(value),
    }
}
fn workspace(name: &str, environment: Option<Uuid>) -> PublishRunOutput {
    PublishRunOutput {
        name: name.into(),
        output: OutputValue::ExecutionWorkspace(ExecutionWorkspace {
            host_id: Uuid::new_v4(),
            workspace_root: "/work".into(),
            path: "task".into(),
            environment_id: environment,
            sandbox_id: None,
        }),
    }
}
fn conflict<T>(result: Result<T, Error>, code: ConflictCode) {
    assert_eq!(result.err().unwrap().conflict(), Some(code));
}
fn status<T>(result: Result<T, Error>, code: u16) {
    assert!(matches!(result, Err(Error::Server(ref e)) if e.status==code));
}

#[sqlx::test(migrations = "./migrations")]
#[ignore = "requires DATABASE_URL; isolated SQLx database"]
async fn outputs_are_explicit_immutable_scoped_and_survive_terminal_runs(pool: PgPool) {
    let app = App::new(pool).await;
    let run = app.start().await;
    let query = RunOutputsQuery::default();
    assert!(
        run.run_outputs(run.lease().run_id, &query)
            .await
            .unwrap()
            .items
            .is_empty()
    );
    let env = app.environment(app.project).await;
    let foreign_env = app.environment(app.other).await;
    status(
        run.publish_output(&command(workspace("primary", Some(foreign_env))))
            .await,
        404,
    );
    status(
        run.publish_output(&command(output("primary", json!(null))))
            .await,
        400,
    );
    status(
        run.publish_output(&command(output("report", json!({"score":"bad"}))))
            .await,
        400,
    );
    let first = command(workspace("primary", Some(env)));
    let published = run.publish_output(&first).await.unwrap();
    assert_eq!(published.project_id, app.project);
    assert_eq!(published.sequence, 1);
    assert_eq!(run.publish_output(&first).await.unwrap(), published);
    assert_eq!(
        run.publish_output(&command(first.body().clone()))
            .await
            .unwrap(),
        published
    );
    conflict(
        run.publish_output(&command(workspace("primary", None)))
            .await,
        ConflictCode::RunOutputImmutable,
    );
    run.publish_output(&command(workspace("secondary", None)))
        .await
        .unwrap();
    run.publish_output(&command(output("report", json!({"score":0.8}))))
        .await
        .unwrap();
    run.publish_output(&command(PublishRunOutput {
        name: "artifact".into(),
        output: OutputValue::Artifact(ArtifactReference {
            artifact_id: Uuid::new_v4(),
            sha256: "a".repeat(64),
            size_bytes: 123,
            media_type: "application/json".into(),
        }),
    }))
    .await
    .unwrap();
    status(
        run.publish_output(&command(output("undeclared", json!(null))))
            .await,
        400,
    );
    let context = run.context(&ContextQuery::default()).await.unwrap();
    assert!(
        context.messages.items.is_empty(),
        "outputs do not enter history"
    );
    let mut finish = Commit::new(context.run.run.version, context.session.current_revision);
    finish.disposition = Disposition::Failed {
        error: json!({"reason":"test terminal state"})
            .as_object()
            .unwrap()
            .clone(),
    };
    run.commit(&command(finish)).await.unwrap();
    assert_eq!(run.publish_output(&first).await.unwrap(), published);
    conflict(
        run.publish_output(&command(output("a", json!(1)))).await,
        ConflictCode::LeaseLost,
    );
    let reader = app.start().await;
    let page = reader
        .run_outputs(run.lease().run_id, &query)
        .await
        .unwrap();
    assert_eq!(page.items.len(), 4);
    assert_eq!(page.items[0], published);
    assert_eq!(page.next_after_sequence, None);
    let foreign_run = app.start_in(app.other, json!({})).await;
    status(
        reader.run_outputs(foreign_run.lease().run_id, &query).await,
        404,
    );
    status(
        foreign_run.run_outputs(run.lease().run_id, &query).await,
        404,
    );
    let response = reqwest::Client::new()
        .get(format!(
            "{}/internal/runs/{}/outputs/{}",
            app.url,
            reader.lease().run_id,
            run.lease().run_id
        ))
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), 401);
    let response = reqwest::Client::new()
        .post(format!(
            "{}/internal/runs/{}/outputs",
            app.url,
            reader.lease().run_id
        ))
        .bearer_auth(TOKEN)
        .header("x-worker-id", app.client.worker_id().to_string())
        .header("x-lease-epoch", reader.lease().lease_epoch)
        .header("idempotency-key", "forged-scope")
        .json(&json!({"name":"a","output":{"kind":"json","value":1},"project_id":app.other}))
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), 422);
}

#[sqlx::test(migrations = "./migrations")]
#[ignore = "requires DATABASE_URL; isolated SQLx database"]
async fn publication_takeover_and_concurrent_names_are_recoverable(pool: PgPool) {
    let app = App::new(pool).await;
    let run = app.start().await;
    let key = command(output("a", json!({"saved":true})));
    let saved = run.publish_output(&key).await.unwrap();
    let second = App::worker(&app.url).await;
    let unrelated = second.run(run.lease()).unwrap();
    conflict(
        unrelated.publish_output(&key).await,
        ConflictCode::LeaseLost,
    );
    conflict(
        unrelated
            .run_outputs(run.lease().run_id, &RunOutputsQuery::default())
            .await,
        ConflictCode::LeaseLost,
    );
    sqlx::query(
        "update runs set lease_expires_at=clock_timestamp()-interval '1 second' where id=$1",
    )
    .bind(run.lease().run_id)
    .execute(&app.pool)
    .await
    .unwrap();
    let replacement = app.claim(&second, run.lease().run_id).await;
    assert_eq!(replacement.publish_output(&key).await.unwrap(), saved);
    assert_eq!(
        run.publish_output(&key).await.unwrap(),
        saved,
        "original issuer may replay only its receipt"
    );
    conflict(
        run.publish_output(&command(output("b", json!(1)))).await,
        ConflictCode::LeaseLost,
    );
    let a = command(output("b", json!(1)));
    let b = command(output("b", json!(2)));
    let (a, b) = tokio::join!(
        replacement.publish_output(&a),
        replacement.publish_output(&b)
    );
    assert_eq!(usize::from(a.is_ok()) + usize::from(b.is_ok()), 1);
    conflict(
        if a.is_err() { a } else { b },
        ConflictCode::RunOutputImmutable,
    );
    let changed = Command::new(key.key().clone(), output("a", json!({"saved":false})));
    conflict(
        replacement.publish_output(&changed).await,
        ConflictCode::IdempotencyKeyConflict,
    );
    let page = replacement
        .run_outputs(run.lease().run_id, &RunOutputsQuery::default())
        .await
        .unwrap();
    assert_eq!(page.items.len(), 2);
    assert_eq!(page.items[1].sequence, 2);
}

#[sqlx::test(migrations = "./migrations")]
#[ignore = "requires DATABASE_URL; isolated SQLx database"]
async fn contracts_freeze_and_validate_zero_one_multiple_environment_inputs(pool: PgPool) {
    let app = App::new(pool).await;
    let legacy = app.start().await;
    let local = app.environment(app.project).await;
    let foreign = app.environment(app.other).await;
    let mut contract = declarations();
    contract["environment_inputs"] = json!([
        {"config_pointer":"/training/environment","cardinality":"single","required":true},
        {"config_pointer":"/evaluators","cardinality":"multiple"}
    ]);
    app.http(
        "PATCH",
        "/internal/harnesses/output-test",
        json!({"harness_contract":contract}),
        200,
    )
    .await;
    for (config, expected) in [
        (json!({}), 400),
        (json!({"training":{"environment":foreign}}), 404),
        (
            json!({"training":{"environment":local},"evaluators":[foreign]}),
            404,
        ),
        (json!({"training":{"environment":[local]}}), 400),
        (
            json!({"training":{"environment":local},"evaluators":"invalid"}),
            400,
        ),
    ] {
        app.http(
            "POST",
            &format!("/api/projects/{}/sessions", app.project),
            json!({"harness_id":"output-test","config_override":config}),
            expected,
        )
        .await;
    }
    let single = app
        .start_in(app.project, json!({"training":{"environment":local}}))
        .await;
    let multiple = app
        .start_in(
            app.project,
            json!({"training":{"environment":local},"evaluators":[local,local]}),
        )
        .await;
    assert_eq!(
        multiple
            .context(&ContextQuery::default())
            .await
            .unwrap()
            .session
            .config["evaluators"],
        json!([local, local])
    );
    // Old catalog writers preserve declarations; explicit {} applies to new sessions.
    let retained = app
        .http(
            "PUT",
            "/internal/harnesses/output-test",
            json!({"name":"Outputs"}),
            200,
        )
        .await;
    assert_eq!(
        retained["harness_contract"]["environment_inputs"]
            .as_array()
            .unwrap()
            .len(),
        2
    );
    app.http(
        "PATCH",
        "/internal/harnesses/output-test",
        json!({"harness_contract":{}}),
        200,
    )
    .await;
    single
        .publish_output(&command(output("report", json!({"score":1}))))
        .await
        .unwrap();
    legacy
        .publish_output(&command(output("report", json!({"score":2}))))
        .await
        .unwrap();
    let empty = app.start().await;
    status(
        empty.publish_output(&command(output("a", json!(1)))).await,
        400,
    );
    assert!(
        empty
            .run_outputs(empty.lease().run_id, &RunOutputsQuery::default())
            .await
            .unwrap()
            .items
            .is_empty()
    );
    let session = single
        .context(&ContextQuery::default())
        .await
        .unwrap()
        .session
        .id;
    assert!(
        sqlx::query("update sessions set harness_contract='{}' where id=$1")
            .bind(session)
            .execute(&app.pool)
            .await
            .is_err()
    );
    app.http("PATCH","/internal/harnesses/output-test",json!({"harness_contract":{"outputs":{"bad":{"kind":"json","value_schema":{"type":"nonexistent"}}}}}),400).await;
}

#[sqlx::test(migrations = "./migrations")]
#[ignore = "requires DATABASE_URL; isolated SQLx database"]
async fn pagination_is_sequence_ordered_and_byte_bounded(pool: PgPool) {
    let app = App::new(pool).await;
    let run = app.start().await;
    for name in ["a", "b", "c", "d"] {
        run.publish_output(&command(output(name, json!("x".repeat(60 * 1024)))))
            .await
            .unwrap();
    }
    let page = run
        .run_outputs(
            run.lease().run_id,
            &RunOutputsQuery {
                limit: Some(50),
                ..Default::default()
            },
        )
        .await
        .unwrap();
    assert_eq!(page.items.len(), 3);
    assert!(serde_json::to_vec(&page).unwrap().len() < 256 * 1024);
    assert_eq!(page.next_after_sequence, Some(3));
    let last = run
        .run_outputs(
            run.lease().run_id,
            &RunOutputsQuery {
                after_sequence: page.next_after_sequence,
                limit: Some(1),
            },
        )
        .await
        .unwrap();
    assert_eq!(last.items[0].name, "d");
    assert_eq!(last.next_after_sequence, None);
    assert!(
        run.run_outputs(
            run.lease().run_id,
            &RunOutputsQuery {
                limit: Some(51),
                ..Default::default()
            }
        )
        .await
        .is_err()
    );
}
