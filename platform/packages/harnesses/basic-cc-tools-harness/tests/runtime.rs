#![cfg(unix)]
//! Full public Harness interface with real Platform/Postgres and execution HTTP.
use axum::{
    Json,
    extract::Path,
    http::{HeaderMap, StatusCode},
    response::IntoResponse,
    routing::{get, post},
};
use basic_cc_tools_harness::{BasicCcToolsHarness, ID, config_schema};
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
        sqlx::query("insert into project_harnesses(project_id,harness_id,enabled) values($1,'basic-cc-tools-harness',true)")
            .bind(project).execute(&pool).await.unwrap();
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
        let mut gateway = ExecutionClientConfig::new(url.parse().unwrap(), "execution-test-token");
        gateway.allow_insecure_http = true;
        let runtime = RuntimeService::new(pool.clone())
            .with_execution(ExecutionClient::new(gateway).unwrap());
        let descriptor = supervisor.descriptor().clone();
        let now = chrono::Utc::now();
        let host_record = json!({"id":host,"kind":"registered","name":"Test host","state":"ready","desired_state":"ready","status_retryable":false,"roots":descriptor.roots,"descriptor":descriptor,"metadata":{},"revision":1,"created_at":now,"updated_at":now});
        let sandbox_script = script.clone();
        let sandbox_host = host.clone();
        let lose_create = Arc::new(AtomicBool::new(false));
        let create_lost = lose_create.clone();
        let app = router(runtime.clone())
            .merge(worker_router(runtime, BOOT))
            .route("/v1/hosts/{id}", get(move || { let value=host_record.clone(); async move { Json(value) } }))
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
            lose_create,
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
    fn harness(&self) -> BasicCcToolsHarness {
        let mut llm = LlmClientConfig::new(self.url.parse().unwrap(), "llm-test-token");
        llm.allow_insecure_http = true;
        let mut execution =
            ExecutionClientConfig::new(self.url.parse().unwrap(), "execution-test-token");
        execution.allow_insecure_http = true;
        BasicCcToolsHarness::new(
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
        let response = self
            .post(
                &format!("/api/projects/{}/sessions", self.project),
                json!({"harness_id":ID,"initial_run":{
                    "expected_session_revision":0,"input":user("Do the work")},"config_override":{
                        "model":{"provider":"openai","id":"gpt-5.6-terra"},"reasoning_level":"high",
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
    let environment = Uuid::now_v7();
    sqlx::query("insert into project_environments(id,project_id,name,type,machine_id,workspace_root,path) values($1,$2,'Tools workspace','machine',$3,$4,'.')")
        .bind(environment).bind(app.project).bind(app.host.as_str().parse::<Uuid>().unwrap())
        .bind(std::fs::canonicalize(app.temp.path().join("workspace")).unwrap().to_str().unwrap())
        .execute(&app.pool).await.unwrap();
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
        ["read", "write", "edit", "bash"]
    );
    assert_eq!(script.requests[1].request.messages.len(), 11);
    assert!(
        matches!(&script.requests[1].request.messages[1], Message::Assistant(message) if message.native_message["preserved"] == true)
    );
    let entries: i64 = sqlx::query_scalar("select count(*) from session_state")
        .fetch_one(&app.pool)
        .await
        .unwrap();
    assert_eq!(
        entries, 2,
        "resolved target and durable workspace publication plan are session-scoped"
    );
    let state: Value =
        sqlx::query_scalar("select value from session_state where key='execution-target'")
            .fetch_one(&app.pool)
            .await
            .unwrap();
    assert_eq!(state["host_id"], json!(app.host));
    let output: Value =
        sqlx::query_scalar("select output from run_outputs where run_id=$1 and name='workspace'")
            .bind(id)
            .fetch_one(&app.pool)
            .await
            .unwrap();
    assert_eq!(output["kind"], "execution_workspace");
    assert_eq!(output["value"]["host_id"], state["host_id"]);
    assert_eq!(output["value"]["workspace_root"], state["workspace_root"]);
    assert_eq!(output["value"]["path"], state["path"]);
    assert_eq!(output["value"]["environment_id"], json!(environment));
    let input: Value = sqlx::query_scalar(
        "select to_jsonb(i) from run_inputs i where kind='user_message' limit 1",
    )
    .fetch_one(&app.pool)
    .await
    .unwrap();
    let linked: bool =
        sqlx::query_scalar("select exists(select 1 from session_messages where message_id=$1)")
            .bind(serde_json::from_value::<Uuid>(input["handling"]["message_id"].clone()).unwrap())
            .fetch_one(&app.pool)
            .await
            .unwrap();
    assert!(linked);
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
            if saved.is_some_and(|saved| !saved["phase"]["plan"]["running"].is_null()) {
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
    let (session, _config): (Uuid, Value) =
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
    let response = app
        .post(
            &format!("/api/sessions/{session}/runs"),
            json!({"expected_session_revision":revision,"input":user("Edit the file")}),
        )
        .await;
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
