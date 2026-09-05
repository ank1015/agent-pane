#![cfg(unix)]
//! Full public Harness interface with real Platform/Postgres and execution HTTP.
use axum::{
    Json,
    extract::Path,
    http::{HeaderMap, StatusCode},
    response::IntoResponse,
    routing::{get, post},
};
use environments_harness::{EnvironmentsHarness, ID, config_schema};
use execution_client::{ExecutionClient, ExecutionClientConfig};
use execution_core::{ExecutionHostId, ExecutionRuntime, OperationContext, RootId};
use execution_supervisor_core::{
    SupervisorConfig, SupervisorLimits, SupervisorRoot, SupervisorRuntime,
};
use execution_wire::{Operation, RequestEnvelope, dispatch_request};
use harness_runtime::{Execution, Harness, Signals};
use llm_client::{
    CompletionRequest, CompletionResponse, LlmClient, LlmClientConfig, Run, RunState,
};
use llm_contracts::*;
use platform_runtime_client::{
    ClientConfig, Command, PlatformClient, RequestKey, RunClient, WorkerRegistration,
    types::{Claim, Heartbeat},
};
use platform_server::runtime::{RuntimeService, router, worker_router};
use serde_json::{Value, json};
use sqlx::PgPool;
use std::{
    collections::{BTreeMap, VecDeque},
    sync::{
        Arc,
        atomic::{AtomicBool, AtomicUsize, Ordering},
    },
    time::Duration,
};
use tokio::sync::{Mutex, watch};
use uuid::Uuid;

const BOOT: &str = "basic-cc-tests-bootstrap-secret-0123456789";

#[derive(Default)]
struct Script {
    responses: VecDeque<RunState>,
    requests: Vec<CompletionRequest>,
    receipts: BTreeMap<String, (CompletionRequest, Run)>,
    jobs: BTreeMap<Uuid, Run>,
}
struct App {
    pool: PgPool,
    temp: tempfile::TempDir,
    url: String,
    project: Uuid,
    host: ExecutionHostId,
    script: Arc<Mutex<Script>>,
    lose_submit: Arc<AtomicBool>,
    lose_write: Arc<AtomicBool>,
    starts: Arc<AtomicUsize>,
    aborts: Arc<AtomicUsize>,
    server: tokio::task::JoinHandle<()>,
}
impl Drop for App {
    fn drop(&mut self) {
        self.server.abort();
    }
}

impl App {
    async fn new(pool: PgPool, responses: Vec<RunState>) -> Self {
        let project = Uuid::now_v7();
        sqlx::query("insert into projects(project_id,name) values($1,'Harness test')")
            .bind(project)
            .execute(&pool)
            .await
            .unwrap();
        let schema: Value = sqlx::query_scalar("select config_schema from harnesses where id=$1")
            .bind(ID)
            .fetch_one(&pool)
            .await
            .unwrap();
        assert_eq!(
            schema,
            config_schema(),
            "migration and package schema must stay in sync"
        );
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().join("workspace");
        std::fs::create_dir_all(&root).unwrap();
        std::fs::write(root.join("file.txt"), "original").unwrap();
        let supervisor = Arc::new(
            SupervisorRuntime::new(SupervisorConfig {
                host_id: ExecutionHostId::generate(),
                state_directory: temp.path().join("state"),
                roots: vec![SupervisorRoot {
                    id: RootId::new("work").unwrap(),
                    name: "Work".into(),
                    path: root,
                    read_only: false,
                }],
                limits: SupervisorLimits {
                    termination_grace_period: Duration::from_millis(50),
                    ..Default::default()
                },
            })
            .await
            .unwrap(),
        );
        let host = supervisor.descriptor().host_id.clone();
        let script = Arc::new(Mutex::new(Script {
            responses: responses.into(),
            ..Default::default()
        }));
        let submit = script.clone();
        let retrieve = script.clone();
        let abort = script.clone();
        let lose_submit = Arc::new(AtomicBool::new(false));
        let lose_write = Arc::new(AtomicBool::new(false));
        let lost = lose_submit.clone();
        let write_lost = lose_write.clone();
        let starts = Arc::new(AtomicUsize::new(0));
        let start_count = starts.clone();
        let aborts = Arc::new(AtomicUsize::new(0));
        let abort_count = aborts.clone();
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("http://{}", listener.local_addr().unwrap());
        let environment_gateway = platform_server::projects::environments::EnvironmentGateway::new(
            url.parse().unwrap(),
            "execution-test-token",
            Duration::from_secs(5),
        )
        .unwrap();
        let runtime = RuntimeService::new(pool.clone()).with_environments(
            platform_server::projects::environments::EnvironmentService::new(
                pool.clone(),
                environment_gateway,
            ),
        );
        let model_host = host.clone();
        let host_descriptor = supervisor.descriptor().clone();
        let app = router(runtime.clone())
            .merge(worker_router(runtime, BOOT))
            .route("/v1/snapshots/{id}", get(|Path(id): Path<Uuid>| async move { Json(json!({"id":id,"state":"ready","desired_state":"ready","deleted_at":null})) }))
            .route(
                "/v1/llm/runs",
                post(
                    move |headers: HeaderMap, Json(request): Json<CompletionRequest>| {
                        let script = submit.clone();
                        let model_host = model_host.clone();
                        let lost = lost.clone();
                        async move {
                            assert_eq!(headers["authorization"], "Bearer llm-test-token");
                            let key = headers["idempotency-key"].to_str().unwrap().to_string();
                            let mut script = script.lock().await;
                            let result = if let Some((old, result)) = script.receipts.get(&key) {
                                assert_eq!(
                                    &request, old,
                                    "recovery must replay the identical request"
                                );
                                result.clone()
                            } else {
                                script.requests.push(request.clone());
                                let mut state = script
                                    .responses
                                    .pop_front()
                                    .expect("unexpected extra LLM generation");
                                let run_id = Uuid::new_v4();
                                match &mut state {
                                    RunState::Succeeded(result) => {
                                        result.request_id = run_id;
                                        for content in &mut result.message.content {
                                            if let AssistantContent::ToolCall { name, arguments: ToolArguments::Object(args), .. } = content {
                                                if matches!(name.as_str(), "read" | "write" | "edit" | "bash") {
                                                    args.entry("host_id".to_string()).or_insert(json!(model_host));
                                                }
                                            }
                                        }
                                    },
                                    RunState::Failed(result) => result.request_id = run_id,
                                    _ => {}
                                }
                                let result = Run {
                                    run_id,
                                    state,
                                    created_at: chrono::Utc::now(),
                                    completed_at: None,
                                    expires_at: None,
                                };
                                script.jobs.insert(result.run_id, result.clone());
                                script.receipts.insert(key, (request, result.clone()));
                                result
                            };
                            if lost.swap(false, Ordering::SeqCst) {
                                return StatusCode::BAD_GATEWAY.into_response();
                            }
                            Json(result).into_response()
                        }
                    },
                ),
            )
            .route(
                "/v1/llm/runs/{id}",
                get(move |Path(id): Path<Uuid>| {
                    let script = retrieve.clone();
                    async move { Json(script.lock().await.jobs[&id].clone()) }
                }),
            )
            .route(
                "/v1/llm/runs/{id}/abort",
                post(move |Path(id): Path<Uuid>| {
                    let script = abort.clone();
                    let count = abort_count.clone();
                    async move {
                        count.fetch_add(1, Ordering::SeqCst);
                        let mut script = script.lock().await;
                        let run = script.jobs.get_mut(&id).unwrap();
                        if matches!(run.state, RunState::Running) {
                            run.state = RunState::Aborted;
                        }
                        Json(run.clone())
                    }
                }),
            )
            .route("/v1/hosts/{id}", get(move |Path(id): Path<Uuid>| {
                let descriptor = host_descriptor.clone();
                async move { Json(json!({"id":id,"kind":"registered","desired_state":"ready","state":"ready","name":"Test machine","deleted_at":null,"descriptor":descriptor,"roots":descriptor.roots,"status_retryable":false,"metadata":{},"revision":1,"created_at":chrono::Utc::now(),"updated_at":chrono::Utc::now()})) }
            }))
            .route(
                "/v1/hosts/{id}/operations",
                post(
                    move |headers: HeaderMap, Json(request): Json<RequestEnvelope>| {
                        let runtime = supervisor.clone();
                        let lost = write_lost.clone();
                        let count = start_count.clone();
                        async move {
                            assert_eq!(headers["authorization"], "Bearer execution-test-token");
                            let write = matches!(request.operation, Operation::FilesystemWrite(_));
                            if matches!(request.operation, Operation::ProcessStart(_)) {
                                count.fetch_add(1, Ordering::SeqCst);
                            }
                            let result = dispatch_request(
                                runtime.as_ref(),
                                &OperationContext::new(),
                                request,
                            )
                            .await;
                            if write && lost.swap(false, Ordering::SeqCst) {
                                return StatusCode::BAD_GATEWAY.into_response();
                            }
                            Json(result).into_response()
                        }
                    },
                ),
            );
        let server = tokio::spawn(async move {
            axum::serve(listener, app).await.unwrap();
        });
        Self {
            pool,
            temp,
            url,
            project,
            host,
            script,
            lose_submit,
            lose_write,
            starts,
            aborts,
            server,
        }
    }
    fn harness(&self) -> EnvironmentsHarness {
        self.harness_with_gateway(&self.url)
    }
    fn harness_with_gateway(&self, gateway: &str) -> EnvironmentsHarness {
        let mut llm = LlmClientConfig::new(self.url.parse().unwrap(), "llm-test-token");
        llm.allow_insecure_http = true;
        let mut execution =
            ExecutionClientConfig::new(gateway.parse().unwrap(), "execution-test-token");
        execution.allow_insecure_http = true;
        EnvironmentsHarness::new(
            LlmClient::new(llm).unwrap(),
            ExecutionClient::new(execution).unwrap(),
            None,
        )
    }
    async fn post(&self, path: &str, body: Value) -> Value {
        let response = reqwest::Client::new()
            .post(format!("{}{path}", self.url))
            .header("idempotency-key", Uuid::new_v4().to_string())
            .json(&body)
            .send()
            .await
            .unwrap();
        let status = response.status();
        let value = response.json::<Value>().await.unwrap();
        assert!(status.is_success(), "{value}");
        value
    }
    async fn create(&self) -> Uuid {
        self.create_with_web(false).await
    }
    async fn create_with_web(&self, web: bool) -> Uuid {
        let response = self
            .post(
                &format!("/api/projects/{}/sessions", self.project),
                json!({"harness_id":ID,"initial_run":{
                    "expected_session_revision":0,"input":user("Do the work"),"config_override":{
                        "model":{"provider":"openai","id":"gpt-5.6-terra"},"reasoning_level":"high",
                        "web_search_enabled":web
                    }
                }}),
            )
            .await;
        serde_json::from_value(response["run"]["id"].clone()).unwrap()
    }
    async fn claim(&self) -> RunClient {
        let client = PlatformClient::new(
            &self.url,
            Uuid::new_v4(),
            Uuid::new_v4().to_string(),
            ClientConfig {
                max_attempts: 1,
                ..Default::default()
            },
        )
        .unwrap();
        client
            .register(
                BOOT,
                &WorkerRegistration {
                    build_id: "test".into(),
                    supported_harnesses: vec![ID.into()],
                    capacity: 1,
                },
            )
            .await
            .unwrap();
        let assignments = client
            .claim(&Command::new(
                RequestKey::new(Uuid::new_v4().to_string()).unwrap(),
                Claim { limit: 1 },
            ))
            .await
            .unwrap();
        let lease = assignments.items.first().expect("ready run").lease();
        client
            .heartbeat(&Heartbeat {
                leases: vec![lease],
            })
            .await
            .unwrap();
        client.run(lease).unwrap()
    }
    async fn activate(&self, client: RunClient) -> platform_runtime_client::Result<()> {
        let (_send, signals) = watch::channel(Signals::default());
        tokio::time::timeout(
            Duration::from_secs(20),
            self.harness().run(Execution { client, signals }),
        )
        .await
        .unwrap()
    }
    async fn expire(&self, id: Uuid) {
        sqlx::query(
            "update runs set lease_expires_at=clock_timestamp()-interval '1 second' where id=$1",
        )
        .bind(id)
        .execute(&self.pool)
        .await
        .unwrap();
    }
    async fn status(&self, id: Uuid) -> String {
        sqlx::query_scalar("select status from runs where id=$1")
            .bind(id)
            .fetch_one(&self.pool)
            .await
            .unwrap()
    }
    async fn messages(&self, id: Uuid) -> Vec<Message> {
        let rows: Vec<Value> = sqlx::query_scalar("select m.message from session_messages sm join messages m on m.id=sm.message_id where sm.run_id=$1 order by sm.revision").bind(id).fetch_all(&self.pool).await.unwrap();
        rows.into_iter()
            .map(|value| serde_json::from_value(value).unwrap())
            .collect()
    }
    async fn wait_requests(&self, count: usize) {
        tokio::time::timeout(Duration::from_secs(10), async {
            loop {
                if self.script.lock().await.requests.len() >= count {
                    break;
                }
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .unwrap();
    }
    async fn finish_pending(&self, mut state: RunState) {
        if let RunState::Succeeded(result) = &mut state {
            for content in &mut result.message.content {
                if let AssistantContent::ToolCall {
                    name,
                    arguments: ToolArguments::Object(args),
                    ..
                } = content
                {
                    if matches!(name.as_str(), "read" | "write" | "edit" | "bash") {
                        args.entry("host_id".to_string())
                            .or_insert(json!(self.host));
                    }
                }
            }
        }
        let mut script = self.script.lock().await;
        let run = script
            .jobs
            .values_mut()
            .find(|run| matches!(run.state, RunState::Running))
            .unwrap();
        if let RunState::Succeeded(result) = &mut state {
            result.request_id = run.run_id;
        }
        run.state = state;
    }
}
fn user(text: &str) -> Value {
    json!({"role":"user","id":Uuid::new_v4().to_string(),"timestamp":0,"content":[{"type":"text","content":text}]})
}
fn call(name: &str, arguments: Value) -> AssistantContent {
    AssistantContent::ToolCall {
        name: name.into(),
        arguments: ToolArguments::Object(arguments.as_object().unwrap().clone()),
        tool_call_id: ToolCallId::new(Uuid::new_v4().to_string()).unwrap(),
    }
}
fn response(content: Vec<AssistantContent>, stop: StopReason) -> RunState {
    RunState::Succeeded(Box::new(CompletionResponse {
        request_id: Uuid::new_v4(),
        account_id: Uuid::new_v4(),
        message: AssistantMessage {
            id: MessageId::new(Uuid::new_v4().to_string()).unwrap(),
            model: ModelRef {
                provider: ProviderId::new("openai").unwrap(),
                id: ModelId::new("gpt-5.6-terra").unwrap(),
                name: None,
            },
            usage: None,
            duration_ms: 1,
            native_message: json!({"preserved":true}),
            content,
            stop_reason: stop,
            timestamp: Timestamp(0),
        },
    }))
}
fn tools(calls: Vec<AssistantContent>) -> RunState {
    response(calls, StopReason::ToolUse)
}
fn final_answer() -> RunState {
    response(
        vec![AssistantContent::Response {
            response: TextContent {
                content: "Done".into(),
                metadata: None,
            },
        }],
        StopReason::Stop,
    )
}

#[sqlx::test(migrations = "../../../apps/server/migrations")]
#[ignore = "requires PostgreSQL with CREATEDB"]
async fn all_four_tools_and_fresh_read_per_edit(pool: PgPool) {
    let app = App::new(
        pool,
        vec![
            tools(vec![
                call(
                    "edit",
                    json!({"file_path":"file.txt","old_string":"original","new_string":"bad"}),
                ),
                call("read", json!({"file_path":"file.txt"})),
                call(
                    "edit",
                    json!({"file_path":"file.txt","old_string":"original","new_string":"first"}),
                ),
                call(
                    "edit",
                    json!({"file_path":"file.txt","old_string":"first","new_string":"bad"}),
                ),
                call("read", json!({"file_path":"file.txt"})),
                call("write", json!({"file_path":"file.txt","content":"second"})),
                call("write", json!({"file_path":"file.txt","content":"bad"})),
                call("write", json!({"file_path":"new.txt","content":"new"})),
                call("bash", json!({"command":"printf 'bash works'"})),
            ]),
            final_answer(),
        ],
    )
    .await;
    let id = app.create().await;
    app.activate(app.claim().await).await.unwrap();
    assert_eq!(app.status(id).await, "completed");
    assert_eq!(app.starts.load(Ordering::SeqCst), 1);
    assert_eq!(
        std::fs::read_to_string(app.temp.path().join("workspace/file.txt")).unwrap(),
        "second"
    );
    assert_eq!(
        std::fs::read_to_string(app.temp.path().join("workspace/new.txt")).unwrap(),
        "new"
    );
    let messages = app.messages(id).await;
    let results: Vec<_> = messages
        .iter()
        .filter_map(|message| {
            if let Message::ToolResult(result) = message {
                Some(result)
            } else {
                None
            }
        })
        .collect();
    assert_eq!(results.len(), 9);
    for (index, result) in results.iter().enumerate() {
        assert_eq!(
            matches!(result.outcome, ToolResultOutcome::Error { .. }),
            [0, 3, 6].contains(&index)
        );
    }
    let script = app.script.lock().await;
    assert_eq!(script.requests.len(), 2);
    assert_eq!(
        script.requests[0]
            .request
            .tools
            .iter()
            .map(ToolDefinition::name)
            .collect::<Vec<_>>(),
        [
            "read",
            "write",
            "edit",
            "bash",
            "list_environments",
            "list_execution_resources",
            "list_snapshots",
            "create_sandbox",
            "snapshot_sandbox",
            "create_environment"
        ]
    );
    assert_eq!(script.requests[1].request.messages.len(), 11);
    assert!(
        matches!(&script.requests[1].request.messages[1], Message::Assistant(message) if message.native_message["preserved"] == true)
    );
    let entries: i64 = sqlx::query_scalar("select count(*) from session_state")
        .fetch_one(&app.pool)
        .await
        .unwrap();
    assert_eq!(entries, 0);
}

#[sqlx::test(migrations = "../../../apps/server/migrations")]
#[ignore = "requires PostgreSQL with CREATEDB"]
async fn lost_llm_response_recovers_identical_request(pool: PgPool) {
    let app = App::new(pool, vec![final_answer()]).await;
    app.lose_submit.store(true, Ordering::SeqCst);
    let id = app.create().await;
    assert!(app.activate(app.claim().await).await.is_err());
    app.expire(id).await;
    app.activate(app.claim().await).await.unwrap();
    assert_eq!(app.status(id).await, "completed");
    assert_eq!(app.script.lock().await.requests.len(), 1);
}

#[sqlx::test(migrations = "../../../apps/server/migrations")]
#[ignore = "requires PostgreSQL with CREATEDB"]
async fn lost_edit_reply_replays_prepared_mutation(pool: PgPool) {
    let app = App::new(
        pool,
        vec![
            tools(vec![
                call("read", json!({"file_path":"file.txt"})),
                call(
                    "edit",
                    json!({"file_path":"file.txt","old_string":"original","new_string":"changed"}),
                ),
            ]),
            final_answer(),
        ],
    )
    .await;
    app.lose_write.store(true, Ordering::SeqCst);
    let id = app.create().await;
    assert!(app.activate(app.claim().await).await.is_err());
    app.expire(id).await;
    app.activate(app.claim().await).await.unwrap();
    assert_eq!(app.status(id).await, "completed");
    assert_eq!(
        std::fs::read_to_string(app.temp.path().join("workspace/file.txt")).unwrap(),
        "changed"
    );
    assert_eq!(
        app.messages(id)
            .await
            .iter()
            .filter(|message| matches!(message, Message::ToolResult(_)))
            .count(),
        2
    );
}

#[sqlx::test(migrations = "../../../apps/server/migrations")]
#[ignore = "requires PostgreSQL with CREATEDB"]
async fn llm_abort_is_called_and_platform_run_is_aborted(pool: PgPool) {
    let app = App::new(pool, vec![RunState::Running]).await;
    let id = app.create().await;
    let (send, signals) = watch::channel(Signals::default());
    let running = tokio::spawn(app.harness().run(Execution {
        client: app.claim().await,
        signals,
    }));
    tokio::time::timeout(Duration::from_secs(10), async {
        loop {
            if !app.script.lock().await.jobs.is_empty() {
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap();
    app.post(&format!("/api/runs/{id}/abort"), json!({})).await;
    send.send(Signals {
        abort_requested: true,
        ..Default::default()
    })
    .unwrap();
    tokio::time::timeout(Duration::from_secs(10), running)
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    assert_eq!(app.status(id).await, "aborted");
    assert_eq!(app.aborts.load(Ordering::SeqCst), 1);
}

#[sqlx::test(migrations = "../../../apps/server/migrations")]
#[ignore = "requires PostgreSQL with CREATEDB"]
async fn truncated_response_fails_without_another_generation(pool: PgPool) {
    let app = App::new(
        pool,
        vec![response(
            vec![AssistantContent::Response {
                response: TextContent {
                    content: "partial".into(),
                    metadata: None,
                },
            }],
            StopReason::Length,
        )],
    )
    .await;
    let id = app.create().await;
    app.activate(app.claim().await).await.unwrap();
    assert_eq!(app.status(id).await, "failed");
    assert_eq!(app.script.lock().await.requests.len(), 1);
}

#[sqlx::test(migrations = "../../../apps/server/migrations")]
#[ignore = "requires PostgreSQL with CREATEDB"]
async fn steering_waits_for_tool_batch_and_continues_after_final_race(pool: PgPool) {
    let app = App::new(pool, vec![RunState::Running, final_answer()]).await;
    let id = app.create().await;
    let (_send, signals) = watch::channel(Signals::default());
    let running = tokio::spawn(app.harness().run(Execution {
        client: app.claim().await,
        signals,
    }));
    app.wait_requests(1).await;
    app.post(
        &format!("/api/runs/{id}/inputs"),
        json!({"kind":"user_message","message":user("One more thing")}),
    )
    .await;
    app.finish_pending(final_answer()).await;
    tokio::time::timeout(Duration::from_secs(10), running)
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    assert_eq!(app.status(id).await, "completed");
    let script = app.script.lock().await;
    assert_eq!(script.requests.len(), 2);
    let history = &script.requests[1].request.messages;
    assert!(matches!(&history[1], Message::Assistant(_)));
    assert!(matches!(&history[2], Message::User(_)));
}

#[sqlx::test(migrations = "../../../apps/server/migrations")]
#[ignore = "requires PostgreSQL with CREATEDB"]
async fn worker_drain_recovers_bash_without_restarting_process(pool: PgPool) {
    let app = App::new(
        pool,
        vec![
            tools(vec![call(
                "bash",
                json!({"command":"printf x >> count; sleep 1; printf done"}),
            )]),
            final_answer(),
        ],
    )
    .await;
    let id = app.create().await;
    let (send, signals) = watch::channel(Signals::default());
    let running = tokio::spawn(app.harness().run(Execution {
        client: app.claim().await,
        signals,
    }));
    tokio::time::timeout(Duration::from_secs(10), async {
        loop {
            let saved: Option<Value> =
                sqlx::query_scalar("select state from run_checkpoints where run_id=$1")
                    .bind(id)
                    .fetch_optional(&app.pool)
                    .await
                    .unwrap();
            if saved.is_some_and(|saved| !saved["phase"]["plan"]["operation"]["running"].is_null())
            {
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap();
    send.send(Signals {
        draining: true,
        ..Default::default()
    })
    .unwrap();
    tokio::time::timeout(Duration::from_secs(10), running)
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    assert_eq!(app.status(id).await, "ready");
    app.activate(app.claim().await).await.unwrap();
    assert_eq!(app.status(id).await, "completed");
    assert_eq!(app.starts.load(Ordering::SeqCst), 1);
    assert_eq!(
        std::fs::read_to_string(app.temp.path().join("workspace/count")).unwrap(),
        "x"
    );
}

#[sqlx::test(migrations = "../../../apps/server/migrations")]
#[ignore = "requires PostgreSQL with CREATEDB"]
async fn external_change_between_read_and_edit_is_preserved(pool: PgPool) {
    let app = App::new(
        pool,
        vec![
            tools(vec![call("read", json!({"file_path":"file.txt"}))]),
            RunState::Running,
            final_answer(),
        ],
    )
    .await;
    let id = app.create().await;
    let (_send, signals) = watch::channel(Signals::default());
    let running = tokio::spawn(app.harness().run(Execution {
        client: app.claim().await,
        signals,
    }));
    app.wait_requests(2).await;
    std::fs::write(
        app.temp.path().join("workspace/file.txt"),
        "external change",
    )
    .unwrap();
    app.finish_pending(tools(vec![call(
        "edit",
        json!({"file_path":"file.txt","old_string":"original","new_string":"overwrite"}),
    )]))
    .await;
    tokio::time::timeout(Duration::from_secs(10), running)
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    assert_eq!(app.status(id).await, "completed");
    assert_eq!(
        std::fs::read_to_string(app.temp.path().join("workspace/file.txt")).unwrap(),
        "external change"
    );
    assert!(app.messages(id).await.iter().any(|message| matches!(message, Message::ToolResult(result) if matches!(result.outcome, ToolResultOutcome::Error { .. }))));
}

#[sqlx::test(migrations = "../../../apps/server/migrations")]
#[ignore = "requires PostgreSQL with CREATEDB"]
async fn followup_run_requires_a_new_read_despite_old_history(pool: PgPool) {
    let app = App::new(
        pool,
        vec![
            tools(vec![call("read", json!({"file_path":"file.txt"}))]),
            final_answer(),
            tools(vec![call(
                "edit",
                json!({"file_path":"file.txt","old_string":"original","new_string":"bad"}),
            )]),
            final_answer(),
        ],
    )
    .await;
    let first = app.create().await;
    app.activate(app.claim().await).await.unwrap();
    let (session, config): (Uuid, Value) =
        sqlx::query_as("select session_id,config from runs where id=$1")
            .bind(first)
            .fetch_one(&app.pool)
            .await
            .unwrap();
    let revision: i64 = sqlx::query_scalar("select current_revision from sessions where id=$1")
        .bind(session)
        .fetch_one(&app.pool)
        .await
        .unwrap();
    let response = app.post(&format!("/api/sessions/{session}/runs"), json!({"expected_session_revision":revision,"input":user("Edit the file"),"config_override":config})).await;
    let second: Uuid = serde_json::from_value(response["run"]["id"].clone()).unwrap();
    app.activate(app.claim().await).await.unwrap();
    assert_eq!(app.status(second).await, "completed");
    assert!(app.messages(second).await.iter().any(|message| matches!(message, Message::ToolResult(result) if matches!(result.outcome, ToolResultOutcome::Error { .. }))));
    assert_eq!(
        std::fs::read_to_string(app.temp.path().join("workspace/file.txt")).unwrap(),
        "original"
    );
    let script = app.script.lock().await;
    assert_eq!(
        script.requests[0].request.provider_options["prompt_cache_key"],
        script.requests[2].request.provider_options["prompt_cache_key"]
    );
}

#[sqlx::test(migrations = "../../../apps/server/migrations")]
#[ignore = "requires PostgreSQL with CREATEDB"]
async fn abort_stops_bash_and_records_unfinished_tool_results(pool: PgPool) {
    let app = App::new(
        pool,
        vec![tools(vec![
            call("bash", json!({"command":"sleep 10; printf bad > after"})),
            call("write", json!({"file_path":"never.txt","content":"bad"})),
        ])],
    )
    .await;
    let id = app.create().await;
    let (send, signals) = watch::channel(Signals::default());
    let running = tokio::spawn(app.harness().run(Execution {
        client: app.claim().await,
        signals,
    }));
    tokio::time::timeout(Duration::from_secs(10), async {
        loop {
            if app.starts.load(Ordering::SeqCst) > 0 {
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap();
    app.post(&format!("/api/runs/{id}/abort"), json!({})).await;
    send.send(Signals {
        abort_requested: true,
        ..Default::default()
    })
    .unwrap();
    tokio::time::timeout(Duration::from_secs(10), running)
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    assert_eq!(app.status(id).await, "aborted");
    assert!(!app.temp.path().join("workspace/never.txt").exists());
    assert!(!app.temp.path().join("workspace/after").exists());
    let results = app.messages(id).await;
    assert_eq!(
        results
            .iter()
            .filter(|message| matches!(message, Message::ToolResult(_)))
            .count(),
        2
    );
    assert_eq!(app.script.lock().await.requests.len(), 1);
}
#[sqlx::test(migrations = "../../../apps/server/migrations")]
#[ignore = "requires PostgreSQL with CREATEDB"]
async fn machine_environment_is_created_and_visible_in_project(pool: PgPool) {
    let app = App::new(pool, vec![]).await;
    app.script.lock().await.responses.extend([
        tools(vec![call("list_environments", json!({})), call("bash", json!({"command":"mkdir -p benchmark && printf ready > benchmark/setup.txt"}))]),
        tools(vec![call("read", json!({"file_path":"benchmark/setup.txt"})), call("create_environment", json!({"name":"Machine benchmark","type":"machine","machine_id":app.host,"workspace_root":"work","path":"benchmark"}))]),
        tools(vec![call("list_environments", json!({}))]), final_answer(),
    ]);
    let id = app.create().await;
    app.activate(app.claim().await).await.unwrap();
    assert_eq!(app.status(id).await, "completed");
    let row: (String, String, String) = sqlx::query_as(
        "select name,path,workspace_root_path from project_environments where project_id=$1",
    )
    .bind(app.project)
    .fetch_one(&app.pool)
    .await
    .unwrap();
    assert_eq!(row.0, "Machine benchmark");
    assert_eq!(row.1, "benchmark");
    assert_eq!(
        row.2,
        app.temp
            .path()
            .join("workspace")
            .canonicalize()
            .unwrap()
            .to_str()
            .unwrap()
    );
    let messages = app.messages(id).await;
    let lists: Vec<_> = messages
        .iter()
        .filter_map(|m| {
            if let Message::ToolResult(r) = m {
                (r.tool_name == "list_environments").then_some(r)
            } else {
                None
            }
        })
        .collect();
    assert_eq!(lists.len(), 2);
    assert!(
        matches!(&lists[1].content[0], ContentPart::Text(t) if t.content.contains("Machine benchmark"))
    );
}

#[sqlx::test(migrations = "../../../apps/server/migrations")]
#[ignore = "requires PostgreSQL with CREATEDB"]
async fn sandbox_snapshot_and_environment_survive_lost_lifecycle_replies(pool: PgPool) {
    use axum::extract::Request;
    let app = App::new(pool, vec![]).await;
    let host = Uuid::parse_str(app.host.as_str()).unwrap();
    let snapshot = Uuid::new_v4();
    let account = Uuid::new_v4();
    let receipts = Arc::new(Mutex::new(BTreeMap::<String, Value>::new()));
    let lost_host = Arc::new(AtomicBool::new(true));
    let lost_snapshot = Arc::new(AtomicBool::new(true));
    let captured = receipts.clone();
    let origin = app.url.clone();
    let root_path = app
        .temp
        .path()
        .join("workspace")
        .to_string_lossy()
        .into_owned();
    let gateway = axum::Router::new().fallback(move |request: Request| {
        let captured = captured.clone(); let origin = origin.clone(); let root_path = root_path.clone();
        let lost_host = lost_host.clone(); let lost_snapshot = lost_snapshot.clone();
        async move {
            assert_eq!(request.headers()["authorization"], "Bearer execution-test-token");
            let path = request.uri().path().to_owned(); let method = request.method().clone();
            let key = request.headers().get("idempotency-key").map(|h|h.to_str().unwrap().to_owned());
            let body = axum::body::to_bytes(request.into_body(), 1024*1024).await.unwrap();
            let payload: Value = if body.is_empty() { json!({}) } else { serde_json::from_slice(&body).unwrap() };
            if path.ends_with("/operations") {
                let response = reqwest::Client::new().post(format!("{origin}{path}")).bearer_auth("execution-test-token").json(&payload).send().await.unwrap();
                let status = response.status(); let body = response.bytes().await.unwrap();
                return (status, [("content-type","application/json")], body).into_response();
            }
            let now = chrono::Utc::now();
            let host_record = json!({"id":host,"kind":"e2b","name":"Builder","state":"ready","desired_state":"ready","status_retryable":false,"roots":[{"id":"work","name":"Work","native_path":root_path,"read_only":false}],"metadata":{},"e2b":{"e2b_account_id":account,"e2b_sandbox_id":"provider-host","source":{"type":"base","e2b_account_id":account},"timeout_seconds":3600},"revision":1,"created_at":now,"updated_at":now});
            let snapshot_record = json!({"id":snapshot,"e2b_account_id":account,"source_host_id":host,"name":"Prepared","state":"ready","desired_state":"ready","metadata":{},"created_at":now,"updated_at":now});
            if method == axum::http::Method::POST {
                let key = key.unwrap();
                let mut receipts = captured.lock().await;
                if let Some(old) = receipts.get(&key) { assert_eq!(old, &payload, "replay must retain the exact creation body"); }
                else { receipts.insert(key, payload); }
                if path == "/v1/hosts" {
                    if lost_host.swap(false, Ordering::SeqCst) { return StatusCode::BAD_GATEWAY.into_response(); }
                    let mut record = host_record; record["state"] = json!("provisioning");
                    return (StatusCode::ACCEPTED, Json(record)).into_response();
                }
                assert!(path.ends_with("/snapshots"));
                if lost_snapshot.swap(false, Ordering::SeqCst) { return StatusCode::BAD_GATEWAY.into_response(); }
                let mut record = snapshot_record; record["state"] = json!("creating");
                return (StatusCode::ACCEPTED, Json(record)).into_response();
            }
            if path == "/v1/hosts" { return Json(json!([])).into_response(); }
            if path == "/v1/e2b-accounts" { return Json(json!([{"id":account,"name":"Personal","credential_fingerprint":"not-model-visible","status":"active","is_default":true,"created_at":now,"updated_at":now}])).into_response(); }
            if path == "/v1/snapshots" { return Json(json!([])).into_response(); }
            if path.starts_with("/v1/snapshots/") { return Json(snapshot_record).into_response(); }
            Json(host_record).into_response()
        }
    });
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let gateway_url = format!("http://{}", listener.local_addr().unwrap());
    let task = tokio::spawn(async move {
        axum::serve(listener, gateway).await.unwrap();
    });
    app.script.lock().await.responses.extend([
        tools(vec![call("list_execution_resources", json!({})), call("list_snapshots", json!({})), call("create_sandbox", json!({"source":{"type":"base","e2b_account_id":account}}))]),
        tools(vec![call("write", json!({"file_path":"prepared.txt","content":"ready"})), call("read", json!({"file_path":"prepared.txt"}))]),
        tools(vec![call("snapshot_sandbox", json!({"host_id":host,"name":"Prepared"}))]),
        tools(vec![call("create_environment", json!({"name":"Sandbox benchmark","type":"sandbox","snapshot_id":snapshot,"workspace_root":"work","path":"."}))]), final_answer(),
    ]);
    let id = app.create().await;
    for attempt in 0..3 {
        let (_send, signals) = watch::channel(Signals::default());
        let result = tokio::time::timeout(
            Duration::from_secs(20),
            app.harness_with_gateway(&gateway_url).run(Execution {
                client: app.claim().await,
                signals,
            }),
        )
        .await
        .unwrap();
        if attempt < 2 {
            assert!(result.is_err());
            app.expire(id).await;
        } else {
            result.unwrap();
        }
    }
    assert_eq!(app.status(id).await, "completed");
    assert_eq!(
        receipts.lock().await.len(),
        2,
        "one sandbox and one snapshot identity"
    );
    let rows: Vec<Uuid> =
        sqlx::query_scalar("select snapshot_id from project_environments where project_id=$1")
            .bind(app.project)
            .fetch_all(&app.pool)
            .await
            .unwrap();
    assert_eq!(rows, [snapshot]);
    assert_eq!(
        std::fs::read_to_string(app.temp.path().join("workspace/prepared.txt")).unwrap(),
        "ready"
    );
    for message in app.messages(id).await {
        if let Message::ToolResult(result) = message {
            assert!(
                matches!(result.outcome, ToolResultOutcome::Success),
                "{}",
                result.tool_name
            );
            assert!(
                !serde_json::to_string(&result)
                    .unwrap()
                    .contains("not-model-visible")
            );
        }
    }
    task.abort();
}
#[sqlx::test(migrations = "../../../apps/server/migrations")]
#[ignore = "requires PostgreSQL with CREATEDB"]
async fn both_web_tools_are_wired_with_schemas_and_usage_details(pool: PgPool) {
    let app = App::new(
        pool,
        vec![
            tools(vec![
                call("search", json!({"query":"runtime setup"})),
                call("scrape", json!({"url":"https://example.com/setup"})),
            ]),
            final_answer(),
        ],
    )
    .await;
    let web = axum::Router::new()
        .route("/search", post(|h: HeaderMap, Json(v): Json<Value>| async move {
            assert_eq!(h["authorization"], "Bearer web-secret"); assert_eq!(v["limit"], 10);
            Json(json!({"success":true,"id":"search-id","creditsUsed":1,"data":{"web":[{"title":"Setup","url":"https://example.com/setup"}]}}))
        }))
        .route("/scrape", post(|h: HeaderMap, Json(v): Json<Value>| async move {
            assert_eq!(h["authorization"], "Bearer web-secret"); assert_eq!(v["formats"], json!(["markdown"]));
            Json(json!({"success":true,"data":{"markdown":"Install dependencies and verify.","metadata":{"sourceURL":"https://example.com/setup"}}}))
        }));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}", listener.local_addr().unwrap());
    let task = tokio::spawn(async move {
        axum::serve(listener, web).await.unwrap();
    });
    let mut llm = LlmClientConfig::new(app.url.parse().unwrap(), "llm-test-token");
    llm.allow_insecure_http = true;
    let mut exec = ExecutionClientConfig::new(app.url.parse().unwrap(), "execution-test-token");
    exec.allow_insecure_http = true;
    let harness = EnvironmentsHarness::new(
        LlmClient::new(llm).unwrap(),
        ExecutionClient::new(exec).unwrap(),
        Some(environments_harness::WebTools {
            search: tool_firecrawl_search::FirecrawlSearchToolContext::with_client(
                "web-secret",
                format!("{url}/search"),
                reqwest::Client::new(),
            )
            .unwrap(),
            scrape: tool_firecrawl_scrape::FirecrawlScrapeToolContext::with_client(
                "web-secret",
                format!("{url}/scrape"),
                reqwest::Client::new(),
            )
            .unwrap(),
        }),
    );
    let id = app.create_with_web(true).await;
    let (_send, signals) = watch::channel(Signals::default());
    tokio::time::timeout(
        Duration::from_secs(20),
        harness.run(Execution {
            client: app.claim().await,
            signals,
        }),
    )
    .await
    .unwrap()
    .unwrap();
    assert_eq!(app.status(id).await, "completed");
    assert_eq!(app.script.lock().await.requests[0].request.tools.len(), 12);
    let messages = app.messages(id).await;
    let search = messages
        .iter()
        .find_map(|m| {
            if let Message::ToolResult(r) = m {
                (r.tool_name == "search").then_some(r)
            } else {
                None
            }
        })
        .unwrap();
    assert_eq!(search.details.as_ref().unwrap()["credits_used"], 1);
    assert!(
        messages
            .iter()
            .all(|m| !serde_json::to_string(m).unwrap().contains("web-secret"))
    );
    task.abort();
}

#[sqlx::test(migrations = "../../../apps/server/migrations")]
#[ignore = "requires PostgreSQL with CREATEDB"]
async fn enabled_web_without_credentials_fails_before_model_dispatch(pool: PgPool) {
    let app = App::new(pool, vec![]).await;
    let id = app.create_with_web(true).await;
    app.activate(app.claim().await).await.unwrap();
    assert_eq!(app.status(id).await, "failed");
    assert!(app.script.lock().await.requests.is_empty());
}
