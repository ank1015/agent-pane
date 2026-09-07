#![cfg(unix)]
//! Full public Harness interface with real Platform/Postgres and execution HTTP.
use axum::{
    Json,
    extract::Path,
    http::{HeaderMap, StatusCode},
    response::IntoResponse,
    routing::{get, post},
};
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
use unified_exec_only_harness::{ID, UnifiedExecOnlyHarness, config_schema};
use uuid::Uuid;

const BOOT: &str = "unified-exec-only-tests-bootstrap-secret-0123456789";

#[derive(Default)]
struct Script {
    host_requests: BTreeMap<String, Value>,
    host_attempts: usize,
    responses: VecDeque<RunState>,
    requests: Vec<CompletionRequest>,
    receipts: BTreeMap<String, (CompletionRequest, Run)>,
    jobs: BTreeMap<Uuid, Run>,
}
struct App {
    lose_create: Arc<AtomicBool>,
    pool: PgPool,
    temp: tempfile::TempDir,
    url: String,
    project: Uuid,
    host: ExecutionHostId,
    script: Arc<Mutex<Script>>,
    lose_submit: Arc<AtomicBool>,
    lose_start: Arc<AtomicBool>,
    lose_input: Arc<AtomicBool>,
    lose_result_commit: Arc<AtomicBool>,
    lose_failure_commit: Arc<AtomicBool>,
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
        sqlx::query(
            "insert into project_harnesses(project_id,harness_id,enabled) values($1,$2,true)",
        )
        .bind(project)
        .bind(ID)
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
        let models: Value =
            sqlx::query_scalar("select supported_models from harnesses where id=$1")
                .bind(ID)
                .fetch_one(&pool)
                .await
                .unwrap();
        assert_eq!(
            models,
            serde_json::to_value(unified_exec_only_harness::supported_models()).unwrap()
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
        let shell_path = format!("{}:/usr/bin:/bin", temp.path().join("workspace").display());
        let script = Arc::new(Mutex::new(Script {
            responses: responses.into(),
            ..Default::default()
        }));
        let submit = script.clone();
        let retrieve = script.clone();
        let abort = script.clone();
        let lose_submit = Arc::new(AtomicBool::new(false));
        let lose_start = Arc::new(AtomicBool::new(false));
        let lose_input = Arc::new(AtomicBool::new(false));
        let start_lost = lose_start.clone();
        let input_lost = lose_input.clone();
        let lost = lose_submit.clone();
        let starts = Arc::new(AtomicUsize::new(0));
        let start_count = starts.clone();
        let aborts = Arc::new(AtomicUsize::new(0));
        let abort_count = aborts.clone();
        let lose_result_commit = Arc::new(AtomicBool::new(false));
        let commit_lost = lose_result_commit.clone();
        let lose_failure_commit = Arc::new(AtomicBool::new(false));
        let failure_lost = lose_failure_commit.clone();
        let runtime = RuntimeService::new(pool.clone());
        let sandbox_script = script.clone();
        let sandbox_host = host.clone();
        let lose_create = Arc::new(AtomicBool::new(false));
        let create_lost = lose_create.clone();
        let app = router(runtime.clone())
            .merge(worker_router(runtime, BOOT))
            .route("/v1/hosts", post(move |headers: HeaderMap, Json(request): Json<Value>| {
                let script = sandbox_script.clone();
                let host = sandbox_host.clone();
                let lost = create_lost.clone();
                async move {
                    let key = headers["idempotency-key"].to_str().unwrap().to_owned();
                    let mut script = script.lock().await;
                    script.host_attempts += 1;
                    if let Some(previous) = script.host_requests.get(&key) { assert_eq!(previous, &request); }
                    else { script.host_requests.insert(key, request); }
                    if lost.swap(false, Ordering::SeqCst) { return StatusCode::BAD_GATEWAY.into_response(); }
                    let now = chrono::Utc::now();
                    Json(json!({"id":host,"kind":"e2b","state":"ready","desired_state":"ready",
                        "status_retryable":false,"roots":[],"metadata":{},"revision":1,"created_at":now,"updated_at":now})).into_response()
                }
            }))
            .route(
                "/v1/llm/runs",
                post(
                    move |headers: HeaderMap, Json(request): Json<CompletionRequest>| {
                        let script = submit.clone();
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
                                    RunState::Succeeded(result) => result.request_id = run_id,
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
            .route(
                "/v1/hosts/{id}/operations",
                post(
                    move |headers: HeaderMap, Json(mut request): Json<RequestEnvelope>| {
                        let runtime = supervisor.clone();
                        let start_lost = start_lost.clone();
                        let input_lost = input_lost.clone();
                        let count = start_count.clone();
                        let shell_path = shell_path.clone();
                        async move {
                            assert_eq!(headers["authorization"], "Bearer execution-test-token");
                            // Model the execution host's configured PATH, without
                            // changing the test process's global environment.
                            if let Operation::ProcessStart(start) = &mut request.operation {
                                start.environment.set.insert("PATH".into(), shell_path);
                            }
                            let start = matches!(request.operation, Operation::ProcessStart(_));
                            let input = matches!(request.operation, Operation::ProcessWrite(_));
                            if matches!(request.operation, Operation::ProcessStart(_)) {
                                count.fetch_add(1, Ordering::SeqCst);
                            }
                            let result = dispatch_request(
                                runtime.as_ref(),
                                &OperationContext::new(),
                                request,
                            )
                            .await;
                            if start && start_lost.swap(false, Ordering::SeqCst) || input && input_lost.swap(false, Ordering::SeqCst) {
                                return StatusCode::BAD_GATEWAY.into_response();
                            }
                            Json(result).into_response()
                        }
                    },
                ),
            );
        let app = app.layer(axum::middleware::from_fn(
            move |request: axum::extract::Request, next: axum::middleware::Next| {
                let lost = commit_lost.clone();
                let failure_lost = failure_lost.clone();
                async move {
                    if !request.uri().path().ends_with("/commits") {
                        return next.run(request).await;
                    }
                    let (parts, body) = request.into_parts();
                    let bytes = axum::body::to_bytes(body, 1024 * 1024).await.unwrap();
                    let value: Value = serde_json::from_slice(&bytes).unwrap();
                    let tool_result = value["messages"].as_array().is_some_and(|messages| {
                        messages
                            .iter()
                            .any(|m| m["message"]["role"] == "tool_result")
                    });
                    let response = next
                        .run(axum::extract::Request::from_parts(
                            parts,
                            axum::body::Body::from(bytes),
                        ))
                        .await;
                    if response.status().is_success()
                        && (tool_result && lost.swap(false, Ordering::SeqCst)
                            || !value["checkpoint"]["state"]["failure"].is_null()
                                && failure_lost.swap(false, Ordering::SeqCst))
                    {
                        return StatusCode::BAD_GATEWAY.into_response();
                    }
                    response
                }
            },
        ));
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("http://{}", listener.local_addr().unwrap());
        let server = tokio::spawn(async move {
            axum::serve(listener, app).await.unwrap();
        });
        Self {
            lose_create,
            pool,
            temp,
            url,
            project,
            host,
            script,
            lose_submit,
            lose_start,
            lose_input,
            lose_result_commit,
            lose_failure_commit,
            starts,
            aborts,
            server,
        }
    }
    fn harness(&self) -> UnifiedExecOnlyHarness {
        let mut llm = LlmClientConfig::new(self.url.parse().unwrap(), "llm-test-token");
        llm.allow_insecure_http = true;
        let mut execution =
            ExecutionClientConfig::new(self.url.parse().unwrap(), "execution-test-token");
        execution.allow_insecure_http = true;
        UnifiedExecOnlyHarness::new(
            LlmClient::new(llm).unwrap(),
            ExecutionClient::new(execution).unwrap(),
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
        self.create_for("openai", "gpt-5.6-terra").await
    }
    async fn create_for(&self, provider: &str, model: &str) -> Uuid {
        let response = self
            .post(
                &format!("/api/projects/{}/sessions", self.project),
                json!({"harness_id":ID,"initial_run":{
                    "expected_session_revision":0,"input":user("Do the work")},"config_override":{
                        "model":{"provider":provider,"id":model},"reasoning_level":"high",
                        "environment":{"type":"machine","machine_id":self.host,"workspace_root":self.temp.path().join("workspace").canonicalize().unwrap(),"path":"."}
                    }
                }),
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
        arguments: match arguments {
            Value::String(raw) => ToolArguments::String(raw),
            Value::Object(value) => ToolArguments::Object(value),
            _ => panic!("invalid arguments"),
        },
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
            native_message: json!({"object":"response", "output": content.iter().map(|part| match part {
                AssistantContent::ToolCall { name, arguments: ToolArguments::String(input), tool_call_id } => json!({"type":"custom_tool_call","name":name,"input":input,"call_id":tool_call_id}),
                AssistantContent::ToolCall { name, arguments, tool_call_id } => json!({"type":"function_call","name":name,"arguments":serde_json::to_string(arguments).unwrap(),"call_id":tool_call_id}),
                _ => json!({"type":"message","role":"assistant","content":[{"type":"output_text","text":"Done"}]}),
            }).collect::<Vec<_>>()}),
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
async fn sandbox_creation_replays_and_followups_reuse_target_but_forks_do_not(pool: PgPool) {
    let app = App::new(pool, vec![final_answer(), final_answer(), final_answer()]).await;
    let snapshot = Uuid::new_v4();
    let created = app.post(&format!("/api/projects/{}/sessions", app.project), json!({
        "harness_id":ID,"config_override":{
            "model":{"provider":"openai","id":"gpt-5.6-terra"},"reasoning_level":"high",
            "environment":{"type":"sandbox","snapshot_id":snapshot,"workspace_root":app.temp.path().join("workspace").canonicalize().unwrap(),"path":"."}
        },"initial_run":{"expected_session_revision":0,"input":user("Start sandbox")}
    })).await;
    let run: Uuid = serde_json::from_value(created["run"]["id"].clone()).unwrap();
    let session: Uuid = serde_json::from_value(created["session"]["id"].clone()).unwrap();
    app.lose_create.store(true, Ordering::SeqCst);
    assert!(app.activate(app.claim().await).await.is_err());
    assert_eq!(app.script.lock().await.requests.len(), 0);
    app.expire(run).await;
    app.activate(app.claim().await).await.unwrap();
    assert_eq!(app.status(run).await, "completed");
    assert_eq!(app.script.lock().await.host_requests.len(), 1);
    assert_eq!(app.script.lock().await.host_attempts, 2);
    let revision: i64 = sqlx::query_scalar("select current_revision from sessions where id=$1")
        .bind(session)
        .fetch_one(&app.pool)
        .await
        .unwrap();
    app.post(
        &format!("/api/sessions/{session}/runs"),
        json!({"expected_session_revision":revision,"input":user("Continue")}),
    )
    .await;
    app.activate(app.claim().await).await.unwrap();
    assert_eq!(
        app.script.lock().await.host_attempts,
        2,
        "followup must use the saved host, not create again"
    );
    app.post(
        &format!("/api/sessions/{session}/forks"),
        json!({"at_revision":revision,
        "initial_run":{"expected_session_revision":revision,"input":user("Fork work")}}),
    )
    .await;
    app.activate(app.claim().await).await.unwrap();
    assert_eq!(
        app.script.lock().await.host_requests.len(),
        2,
        "fork has its own durable creation identity"
    );
    assert_eq!(app.script.lock().await.host_attempts, 3);
    for request in app.script.lock().await.host_requests.values() {
        assert_eq!(
            request["source"],
            json!({"type":"snapshot","snapshot_id":snapshot})
        );
    }
    let states: i64 =
        sqlx::query_scalar("select count(*) from session_state where key='execution-target'")
            .fetch_one(&app.pool)
            .await
            .unwrap();
    assert_eq!(states, 2);
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
async fn process_start_and_stdin_reply_loss_do_not_repeat_effects(pool: PgPool) {
    let app = App::new(pool, vec![tools(vec![
        call("exec_command",json!({"cmd":"printf x >> starts; read line; printf '%s' \"$line\" >> received", "tty":true,"login":false,"yield_time_ms":250})),
        call("write_stdin",json!({"session_id":1,"chars":"hello\n","yield_time_ms":1000})),
    ]),final_answer()]).await;
    let run = app.create().await;
    app.lose_start.store(true, Ordering::SeqCst);
    assert!(app.activate(app.claim().await).await.is_err());
    app.expire(run).await;
    app.lose_input.store(true, Ordering::SeqCst);
    assert!(app.activate(app.claim().await).await.is_err());
    app.expire(run).await;
    app.activate(app.claim().await).await.unwrap();
    assert_eq!(app.status(run).await, "completed");
    assert_eq!(
        std::fs::read_to_string(app.temp.path().join("workspace/starts")).unwrap(),
        "x"
    );
    assert_eq!(
        std::fs::read_to_string(app.temp.path().join("workspace/received")).unwrap(),
        "hello"
    );
}

#[sqlx::test(migrations = "../../../apps/server/migrations")]
#[ignore = "requires PostgreSQL with CREATEDB"]
async fn followup_reuses_process_and_fork_rejects_inherited_alias(pool: PgPool) {
    let app=App::new(pool,vec![tools(vec![call("exec_command",json!({"cmd":"printf READY; read line; printf 'FOLLOWUP:%s' \"$line\"","tty":true,"login":false,"yield_time_ms":250}))]),final_answer(),
        tools(vec![call("write_stdin",json!({"session_id":1,"chars":"later\n","yield_time_ms":1000}))]),final_answer(),
        tools(vec![call("write_stdin",json!({"session_id":1,"chars":""})),call("exec_command",json!({"cmd":"printf NEW","login":false}))]),final_answer()]).await;
    let run = app.create().await;
    app.activate(app.claim().await).await.unwrap();
    let session: Uuid = sqlx::query_scalar("select session_id from runs where id=$1")
        .bind(run)
        .fetch_one(&app.pool)
        .await
        .unwrap();
    let revision: i64 = sqlx::query_scalar("select current_revision from sessions where id=$1")
        .bind(session)
        .fetch_one(&app.pool)
        .await
        .unwrap();
    app.post(
        &format!("/api/sessions/{session}/runs"),
        json!({"expected_session_revision":revision,"input":user("Continue")}),
    )
    .await;
    app.activate(app.claim().await).await.unwrap();
    {
        let script = app.script.lock().await;
        let body = provider_openai::build_response_request(&script.requests[3].request).unwrap();
        assert!(body.to_string().contains("FOLLOWUP:later"));
    }
    app.post(&format!("/api/sessions/{session}/forks"),json!({"at_revision":revision,"initial_run":{"expected_session_revision":revision,"input":user("Fork")}})).await;
    app.activate(app.claim().await).await.unwrap();
    let script = app.script.lock().await;
    let results: Vec<_> = script.requests[5]
        .request
        .messages
        .iter()
        .filter_map(|m| {
            if let Message::ToolResult(r) = m {
                Some(r)
            } else {
                None
            }
        })
        .collect();
    let last = &results[results.len() - 2..];
    assert!(matches!(last[0].outcome, ToolResultOutcome::Error { .. }));
    assert_eq!(last[1].details.as_ref().unwrap()["exec_session_id"], 2);
}

#[sqlx::test(migrations = "../../../apps/server/migrations")]
#[ignore = "requires PostgreSQL with CREATEDB"]
async fn drain_preserves_running_command_and_abort_cleans_it_up(pool: PgPool) {
    let app = App::new(
        pool,
        vec![
            tools(vec![call(
                "exec_command",
                json!({"cmd":"printf START; sleep 30","login":false,"yield_time_ms":30000}),
            )]),
            final_answer(),
        ],
    )
    .await;
    let run = app.create().await;
    let client = app.claim().await;
    let (send, signals) = watch::channel(Signals::default());
    let task = tokio::spawn(app.harness().run(Execution { client, signals }));
    tokio::time::timeout(Duration::from_secs(10), async {
        while app.starts.load(Ordering::SeqCst) == 0 {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap();
    send.send_modify(|s| s.draining = true);
    task.await.unwrap().unwrap();
    assert_eq!(app.status(run).await, "ready");
    app.post(&format!("/api/runs/{run}/abort"), json!({})).await;
    app.activate(app.claim().await).await.unwrap();
    assert_eq!(app.status(run).await, "aborted");
    assert_eq!(app.starts.load(Ordering::SeqCst), 1);
    let results = app.messages(run).await;
    assert!(results.iter().any(
        |m| matches!(m,Message::ToolResult(r) if matches!(r.outcome,ToolResultOutcome::Error {..}))
    ));
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
async fn lost_output_commit_keeps_history_and_session_cursor_together(pool: PgPool) {
    let app=App::new(pool,vec![tools(vec![
        call("exec_command",json!({"cmd":"printf FIRST; read line; printf SECOND","tty":true,"login":false,"yield_time_ms":250})),
        call("write_stdin",json!({"session_id":1,"chars":"go\n","yield_time_ms":1000})),
    ]),final_answer()]).await;
    let run = app.create().await;
    app.lose_result_commit.store(true, Ordering::SeqCst);
    assert!(app.activate(app.claim().await).await.is_err());
    app.expire(run).await;
    app.activate(app.claim().await).await.unwrap();
    assert_eq!(app.status(run).await, "completed");
    let messages = app.messages(run).await;
    let results: Vec<_> = messages
        .iter()
        .filter_map(|m| {
            if let Message::ToolResult(r) = m {
                Some(r)
            } else {
                None
            }
        })
        .collect();
    assert_eq!(results.len(), 2);
    assert!(serde_json::to_string(results[0]).unwrap().contains("FIRST"));
    assert!(!serde_json::to_string(results[1]).unwrap().contains("FIRST"));
    assert!(
        serde_json::to_string(results[1])
            .unwrap()
            .contains("SECOND")
    );
    assert_eq!(app.starts.load(Ordering::SeqCst), 1);
}

#[sqlx::test(migrations = "../../../apps/server/migrations")]
#[ignore = "requires PostgreSQL with CREATEDB"]
async fn invalid_assistant_failure_survives_lost_commit_response(pool: PgPool) {
    let app = App::new(
        pool,
        vec![response(
            vec![AssistantContent::Response {
                response: TextContent {
                    content: "Partial".into(),
                    metadata: None,
                },
            }],
            StopReason::Length,
        )],
    )
    .await;
    let run = app.create().await;
    app.lose_failure_commit.store(true, Ordering::SeqCst);
    assert!(app.activate(app.claim().await).await.is_err());
    app.expire(run).await;
    app.activate(app.claim().await).await.unwrap();
    assert_eq!(app.status(run).await, "failed");
    assert_eq!(app.script.lock().await.requests.len(), 1);
}

#[sqlx::test(migrations = "../../../apps/server/migrations")]
#[ignore = "requires PostgreSQL with CREATEDB"]
async fn shell_file_operations_and_interactive_input_use_only_two_tools(pool: PgPool) {
    let app=App::new(pool,vec![tools(vec![
        call("exec_command",json!({"cmd":"printf 'first\n' > doc.txt; sed 's/first/second/' doc.txt > doc.tmp; mv doc.tmp doc.txt; cat doc.txt; head -c 100000 /dev/zero > big.txt; wc -c < big.txt","shell":"/bin/sh","login":false})),
        call("exec_command",json!({"cmd":"printf READY; read line; printf 'GOT:%s' \"$line\"","tty":true,"login":false,"yield_time_ms":250})),
        call("write_stdin",json!({"session_id":2,"chars":"hello\n","yield_time_ms":1000})),
    ]),final_answer()]).await;
    let run = app.create().await;
    app.activate(app.claim().await).await.unwrap();
    assert_eq!(app.status(run).await, "completed");
    assert_eq!(
        std::fs::read_to_string(app.temp.path().join("workspace/doc.txt")).unwrap(),
        "second\n"
    );
    assert_eq!(
        std::fs::metadata(app.temp.path().join("workspace/big.txt"))
            .unwrap()
            .len(),
        100000
    );
    let script = app.script.lock().await;
    for request in &script.requests {
        assert_eq!(
            request
                .request
                .tools
                .iter()
                .map(ToolDefinition::name)
                .collect::<Vec<_>>(),
            vec!["exec_command", "write_stdin"]
        );
        assert!(
            request
                .request
                .tools
                .iter()
                .all(|tool| matches!(tool, ToolDefinition::Function(_)))
        );
        assert!(
            request
                .request
                .instructions
                .as_ref()
                .unwrap()
                .contains("Perform all filesystem work through shell commands")
        );
    }
    let body = provider_openai::build_response_request(&script.requests[1].request).unwrap();
    assert!(body["input"].as_array().unwrap().iter().any(|item| {
        item["output"]
            .as_str()
            .is_some_and(|text| text.contains("GOT:hello"))
    }));
    assert_eq!(app.starts.load(Ordering::SeqCst), 2);
}

#[sqlx::test(migrations = "../../../apps/server/migrations")]
#[ignore = "requires PostgreSQL with CREATEDB"]
async fn shell_patch_command_uses_installed_executable_without_interception(pool: PgPool) {
    use std::os::unix::fs::PermissionsExt;
    let app=App::new(pool,vec![tools(vec![call("exec_command",json!({"cmd":"apply_patch <<'PATCH'\n*** Begin Patch\n*** Add File: unwanted.txt\n+bad\n*** End Patch\nPATCH","shell":"/bin/sh","login":false}))]),final_answer()]).await;
    let helper = app.temp.path().join("workspace/apply_patch");
    std::fs::write(
        &helper,
        "#!/bin/sh\ncat > shell-input\nprintf shell-handler\n",
    )
    .unwrap();
    std::fs::set_permissions(&helper, std::fs::Permissions::from_mode(0o755)).unwrap();
    let run = app.create().await;
    app.activate(app.claim().await).await.unwrap();
    assert_eq!(app.status(run).await, "completed");
    assert_eq!(app.starts.load(Ordering::SeqCst), 1);
    assert!(!app.temp.path().join("workspace/unwanted.txt").exists());
    assert!(
        std::fs::read_to_string(app.temp.path().join("workspace/shell-input"))
            .unwrap()
            .contains("*** Begin Patch")
    );
}

#[sqlx::test(migrations = "../../../apps/server/migrations")]
#[ignore = "requires PostgreSQL with CREATEDB"]
async fn unavailable_tool_names_fail_without_filesystem_effects(pool: PgPool) {
    let app = App::new(
        pool,
        vec![
            tools(vec![call(
                "apply_patch",
                json!("*** Begin Patch\n*** Add File: unexpected\n+bad\n*** End Patch"),
            )]),
            tools(vec![call("view_image", json!({"path":"file.txt"}))]),
        ],
    )
    .await;
    for _ in 0..2 {
        let run = app.create().await;
        app.activate(app.claim().await).await.unwrap();
        assert_eq!(app.status(run).await, "failed");
    }
    assert_eq!(app.starts.load(Ordering::SeqCst), 0);
    assert!(!app.temp.path().join("workspace/unexpected").exists());
}

fn as_fireworks(mut state: RunState, model: &str) -> RunState {
    let RunState::Succeeded(completion) = &mut state else {
        panic!("expected assistant response")
    };
    completion.message.model.provider = ProviderId::new("fireworks").unwrap();
    completion.message.model.id = ModelId::new(model).unwrap();
    let calls:Vec<_>=completion.message.content.iter().filter_map(|part| match part {
        AssistantContent::ToolCall {name,arguments,tool_call_id} => Some(json!({"id":tool_call_id,"type":"function","function":{"name":name,"arguments":serde_json::to_string(arguments).unwrap()}})),
        _ => None,
    }).collect();
    let mut native = json!({"role":"assistant","content":if calls.is_empty() {json!("Done")} else {Value::Null},"reasoning_content":"preserved Fireworks reasoning"});
    if !calls.is_empty() {
        native["tool_calls"] = json!(calls);
    }
    completion.message.native_message = json!({"choices":[{"index":0,"message":native}]});
    state
}

#[sqlx::test(migrations = "../../../apps/server/migrations")]
#[ignore = "requires PostgreSQL with CREATEDB"]
async fn fireworks_executes_shell_tools_and_replays_native_reasoning(pool: PgPool) {
    let model = provider_fireworks::FIREWORKS_MODELS[0].id;
    let app=App::new(pool,vec![as_fireworks(tools(vec![
        call("exec_command",json!({"cmd":"printf FIREWORKS; read line; printf 'INPUT:%s' \"$line\"","tty":true,"login":false,"yield_time_ms":250})),
        call("write_stdin",json!({"session_id":1,"chars":"ok\n","yield_time_ms":1000})),
    ]),model),as_fireworks(final_answer(),model)]).await;
    let run = app.create_for("fireworks", model).await;
    app.activate(app.claim().await).await.unwrap();
    assert_eq!(app.status(run).await, "completed");
    let script = app.script.lock().await;
    let request = &script.requests[1].request;
    assert_eq!(
        request.provider_options["reasoning_effort"],
        provider_fireworks::reasoning_effort(model, "high").unwrap()
    );
    assert_eq!(
        request.provider_options["max_tokens"],
        provider_fireworks::find_model(model).unwrap().max_tokens
    );
    let body = provider_fireworks::build_chat_completion_request(request).unwrap();
    assert_eq!(body["tools"].as_array().unwrap().len(), 2);
    assert_eq!(
        body["messages"][2]["reasoning_content"],
        "preserved Fireworks reasoning"
    );
    assert!(body["messages"].as_array().unwrap().iter().any(|message| {
        message["role"] == "tool"
            && message["content"]
                .as_str()
                .is_some_and(|text| text.contains("INPUT:ok"))
    }));
}
