use std::{
    sync::{
        Arc, Mutex,
        atomic::{AtomicU64, AtomicUsize, Ordering},
    },
    time::Duration,
};

use agent_contracts::{
    AppendRunMessages, ClaimRun, ClaimedRun, CompleteRunTurn, HarnessRevision,
    HarnessRevisionStatus, Run, RunLease, RunMessagesAppended, RunStatus, RunTurnCompleted,
    SessionMessage, SessionMessageDelivery, SessionMessageOrigin, SessionMessagePage,
    TurnCompletionDisposition,
};
use axum::{
    Json, Router,
    extract::{Path, State},
    http::StatusCode,
    response::{IntoResponse, Response},
    routing::{get, post},
};
use chrono::{Duration as ChronoDuration, Utc};
use execution_gateway_client::ExecutionGatewayConfig;
use execution_runtime::OperationContext;
use llm_contracts::Message;
use serde_json::{Value, json};
use url::Url;
use uuid::Uuid;
use worker_pi::{
    clients::{AgentClient, ExecutionClient, LlmGatewayClient},
    config::{AgentServiceConfig, LlmGatewayServiceConfig},
    runtime::{PiRuntime, RetryPolicy, RunOutcome},
    worker::WorkerService,
};

#[tokio::test]
async fn runs_a_model_turn_with_retry_and_commits_the_final_assistant() {
    let agent_state = Arc::new(AgentState::new());
    let agent = Router::new()
        .route("/v1/worker/runs/claim", post(claim))
        .route(
            "/v1/worker/runs/{run_id}/messages",
            get(messages).post(append_messages),
        )
        .route("/v1/worker/runs/{run_id}/complete", post(complete_turn))
        .with_state(agent_state.clone());
    let llm_state = Arc::new(LlmState::default());
    let llm = Router::new()
        .route("/v1/complete", post(llm_complete))
        .with_state(llm_state.clone());

    let agent_client = AgentClient::new(AgentServiceConfig {
        base_url: serve(agent).await,
        worker_token: "worker-secret".to_owned(),
        request_timeout: Duration::from_secs(2),
    })
    .expect("Agent client");
    let worker = WorkerService::new(
        agent_client,
        "pi-worker-test".to_owned(),
        vec!["pi-v1".to_owned()],
    );
    let llm_client = LlmGatewayClient::new(LlmGatewayServiceConfig {
        base_url: serve(llm).await,
        request_timeout: Duration::from_secs(2),
    })
    .expect("LLM client");
    let execution_client = ExecutionClient::new(ExecutionGatewayConfig::new(
        "http://127.0.0.1:1".parse().expect("execution URL"),
        "execution-secret",
    ))
    .expect("execution client");
    let runtime = PiRuntime::new(llm_client, execution_client).with_retry_policy(RetryPolicy {
        max_retries: 1,
        base_delay: Duration::from_millis(1),
        max_retry_delay: Duration::from_millis(5),
    });
    let shutdown = OperationContext::new();
    let active = worker.claim_next(&shutdown).await.expect("claimed run");

    let outcome = runtime.execute(active).await.expect("runtime outcome");

    let RunOutcome::Completed(completed) = outcome else {
        panic!("turn should complete")
    };
    assert_eq!(completed.run.status, RunStatus::Completed);
    assert_eq!(llm_state.calls.load(Ordering::Acquire), 2);

    let requests = llm_state.requests.lock().expect("LLM requests");
    assert_eq!(requests.len(), 2);
    let request = &requests[1];
    assert_eq!(request["account_id"], agent_state.account_id.to_string());
    assert_eq!(request["request"]["model"]["provider"], "openai");
    assert_eq!(request["request"]["model"]["id"], "gpt-5.6-sol");
    assert_eq!(request["request"]["messages"].as_array().unwrap().len(), 1);
    assert_eq!(request["request"]["tools"].as_array().unwrap().len(), 4);
    assert!(
        request["request"]["instructions"]
            .as_str()
            .unwrap()
            .ends_with("\n\nUse Rust 2024 conventions.")
    );
    assert_eq!(
        request["request"]["provider_options"]["prompt_cache_key"],
        agent_state.session_id.to_string()
    );
    assert_eq!(
        request["request"]["provider_options"]["reasoning"]["effort"],
        "high"
    );
    drop(requests);

    let appends = agent_state.appends.lock().expect("appends");
    assert_eq!(appends.len(), 1);
    assert_eq!(appends[0].expected_session_revision, 1);
    let assistant_id = appends[0].messages[0].session_message_id;
    assert!(matches!(
        appends[0].messages[0].message,
        Message::Assistant(_)
    ));
    assert_eq!(
        agent_state
            .completion
            .lock()
            .expect("completion")
            .as_ref()
            .unwrap()
            .final_message_id,
        Some(assistant_id)
    );
}

#[tokio::test]
async fn executes_tools_commits_their_results_and_continues_the_run() {
    let agent_state = Arc::new(AgentState::new());
    let agent = Router::new()
        .route("/v1/worker/runs/claim", post(claim))
        .route(
            "/v1/worker/runs/{run_id}/messages",
            get(messages).post(append_messages),
        )
        .route("/v1/worker/runs/{run_id}/complete", post(complete_turn))
        .with_state(agent_state.clone());
    let llm_state = Arc::new(LlmState {
        tool_call: true,
        ..LlmState::default()
    });
    let llm = Router::new()
        .route("/v1/complete", post(llm_complete))
        .with_state(llm_state);
    let execution_state = Arc::new(ExecutionState::default());
    let execution = Router::new()
        .route("/v1/machines/{machine_id}", get(machine))
        .route(
            "/v1/machines/{machine_id}/operations",
            post(execute_operation),
        )
        .with_state(execution_state.clone());

    let agent_client = AgentClient::new(AgentServiceConfig {
        base_url: serve(agent).await,
        worker_token: "worker-secret".to_owned(),
        request_timeout: Duration::from_secs(2),
    })
    .expect("Agent client");
    let worker = WorkerService::new(
        agent_client,
        "pi-worker-test".to_owned(),
        vec!["pi-v1".to_owned()],
    );
    let llm_client = LlmGatewayClient::new(LlmGatewayServiceConfig {
        base_url: serve(llm).await,
        request_timeout: Duration::from_secs(2),
    })
    .expect("LLM client");
    let execution_client = ExecutionClient::new(ExecutionGatewayConfig::new(
        serve(execution).await,
        "execution-secret",
    ))
    .expect("execution client");
    let runtime = PiRuntime::new(llm_client, execution_client).with_retry_policy(RetryPolicy {
        max_retries: 1,
        base_delay: Duration::from_millis(1),
        max_retry_delay: Duration::from_millis(5),
    });
    let shutdown = OperationContext::new();
    let active = worker.claim_next(&shutdown).await.expect("claimed run");

    let outcome = runtime.execute(active).await.expect("runtime outcome");

    let RunOutcome::Continued(continued) = outcome else {
        panic!("tool turn should continue")
    };
    assert_eq!(continued.run.status, RunStatus::Queued);
    let appends = agent_state.appends.lock().expect("appends");
    assert_eq!(appends.len(), 2);
    assert!(matches!(
        appends[0].messages[0].message,
        Message::Assistant(_)
    ));
    assert!(matches!(
        appends[1].messages[0].message,
        Message::ToolResult(_)
    ));
    assert_eq!(appends[1].expected_session_revision, 2);
    let completion = agent_state.completion.lock().expect("completion");
    assert_eq!(
        completion.as_ref().unwrap().disposition,
        TurnCompletionDisposition::Continue
    );
    assert_eq!(completion.as_ref().unwrap().final_message_id, None);
    let operation = execution_state.operation.lock().expect("operation");
    assert_eq!(operation.as_ref().unwrap()["operation"], "mutation_apply");
    assert_eq!(
        operation.as_ref().unwrap()["request"]["plan"]["operations"][0]["path"]["path"],
        "project/generated.txt"
    );
}

struct AgentState {
    run_id: Uuid,
    session_id: Uuid,
    trigger_message_id: Uuid,
    trigger_session_message_id: Uuid,
    account_id: Uuid,
    revision: AtomicU64,
    appends: Mutex<Vec<AppendRunMessages>>,
    completion: Mutex<Option<CompleteRunTurn>>,
}

impl AgentState {
    fn new() -> Self {
        Self {
            run_id: Uuid::now_v7(),
            session_id: Uuid::now_v7(),
            trigger_message_id: Uuid::now_v7(),
            trigger_session_message_id: Uuid::now_v7(),
            account_id: Uuid::now_v7(),
            revision: AtomicU64::new(1),
            appends: Mutex::new(Vec::new()),
            completion: Mutex::new(None),
        }
    }

    fn run(&self, status: RunStatus, final_message_id: Option<Uuid>) -> Run {
        let now = Utc::now();
        Run {
            run_id: self.run_id,
            session_id: self.session_id,
            trigger_message_id: self.trigger_message_id,
            harness_revision_id: "pi-v1".to_owned(),
            resolved_config: json!({
                "provider": "openai",
                "model_id": "gpt-5.6-sol",
                "reasoning_level": "high",
                "execution": {
                    "machine_id": "machine-a",
                    "workspace_root_id": "root",
                    "cwd": "project"
                },
                "account_id": self.account_id,
                "external_prompt": "Use Rust 2024 conventions.",
                "is_replaced": false
            })
            .as_object()
            .unwrap()
            .clone(),
            status,
            current_turn: 1,
            max_turns: 100,
            failures_in_current_turn: 0,
            max_failures_per_turn: 3,
            state_version: if status == RunStatus::Running { 1 } else { 2 },
            queued_at: (status == RunStatus::Queued).then_some(now),
            final_message_id,
            failure: None,
            created_at: now,
            started_at: Some(now),
            finished_at: (status == RunStatus::Completed).then_some(now),
        }
    }

    fn trigger(&self) -> SessionMessage {
        SessionMessage {
            session_message_id: self.trigger_session_message_id,
            session_id: self.session_id,
            revision: 1,
            message: serde_json::from_value(json!({
                "role": "user",
                "id": "user-1",
                "timestamp": 1,
                "content": [{"type": "text", "content": "Inspect the project."}]
            }))
            .expect("trigger message"),
            origin: SessionMessageOrigin::External,
            delivery: SessionMessageDelivery::Immediate,
            run_id: None,
            turn_number: None,
            created_at: Utc::now(),
            committed_at: Utc::now(),
        }
    }
}

async fn claim(
    State(state): State<Arc<AgentState>>,
    Json(command): Json<ClaimRun>,
) -> Json<ClaimedRun> {
    let now = Utc::now();
    Json(ClaimedRun {
        run: state.run(RunStatus::Running, None),
        lease: RunLease {
            lease_id: command.lease_id,
            run_id: state.run_id,
            lease_version: 1,
            worker_instance_id: command.worker_instance_id,
            acquired_at: now,
            expires_at: now + ChronoDuration::seconds(120),
        },
        harness_revision: HarnessRevision {
            harness_revision_id: "pi-v1".to_owned(),
            harness_id: "pi".to_owned(),
            revision: "1".to_owned(),
            contract_version: 1,
            status: HarnessRevisionStatus::Active,
            default_config: serde_json::Map::new(),
            config_schema: None,
            first_activated_at: Some(now),
            retired_at: None,
            created_at: now,
        },
        current_session_revision: state.revision.load(Ordering::Acquire),
        resume: None,
    })
}

async fn messages(
    State(state): State<Arc<AgentState>>,
    Path(run_id): Path<Uuid>,
) -> Json<SessionMessagePage> {
    assert_eq!(run_id, state.run_id);
    Json(SessionMessagePage {
        items: vec![state.trigger()],
        next_after_revision: None,
    })
}

async fn append_messages(
    State(state): State<Arc<AgentState>>,
    Path(run_id): Path<Uuid>,
    Json(command): Json<AppendRunMessages>,
) -> Json<RunMessagesAppended> {
    assert_eq!(run_id, state.run_id);
    let mut revision = state.revision.load(Ordering::Acquire);
    assert_eq!(command.expected_session_revision, revision);
    let items = command
        .messages
        .iter()
        .map(|new| {
            revision += 1;
            SessionMessage {
                session_message_id: new.session_message_id,
                session_id: state.session_id,
                revision,
                message: new.message.clone(),
                origin: SessionMessageOrigin::Harness,
                delivery: SessionMessageDelivery::Immediate,
                run_id: Some(state.run_id),
                turn_number: Some(1),
                created_at: Utc::now(),
                committed_at: Utc::now(),
            }
        })
        .collect();
    state.revision.store(revision, Ordering::Release);
    state.appends.lock().expect("appends").push(command);
    Json(RunMessagesAppended {
        items,
        current_session_revision: revision,
    })
}

async fn complete_turn(
    State(state): State<Arc<AgentState>>,
    Path(run_id): Path<Uuid>,
    Json(command): Json<CompleteRunTurn>,
) -> Json<RunTurnCompleted> {
    assert_eq!(run_id, state.run_id);
    let final_message_id = command.final_message_id;
    let status = match command.disposition {
        TurnCompletionDisposition::Complete => RunStatus::Completed,
        TurnCompletionDisposition::Continue => RunStatus::Queued,
    };
    *state.completion.lock().expect("completion") = Some(command);
    Json(RunTurnCompleted {
        run: state.run(status, final_message_id),
        committed_messages: Vec::new(),
    })
}

#[derive(Default)]
struct LlmState {
    calls: AtomicUsize,
    requests: Mutex<Vec<Value>>,
    tool_call: bool,
}

async fn llm_complete(State(state): State<Arc<LlmState>>, Json(request): Json<Value>) -> Response {
    state.requests.lock().expect("requests").push(request);
    if state.calls.fetch_add(1, Ordering::AcqRel) == 0 {
        return (
            StatusCode::SERVICE_UNAVAILABLE,
            Json(json!({
                "error": {
                    "kind": "provider",
                    "message": "provider is temporarily unavailable",
                    "can_retry": true,
                    "retry_after_ms": 0,
                    "provider_error": null
                }
            })),
        )
            .into_response();
    }

    let (content, stop_reason) = if state.tool_call {
        (
            json!([{
                "type": "tool_call",
                "name": "write",
                "arguments": {"path": "generated.txt", "content": "hello"},
                "tool_call_id": "call-1"
            }]),
            "tool_use",
        )
    } else {
        (
            json!([{
                "type": "response",
                "response": {"content": "The project is ready."}
            }]),
            "stop",
        )
    };
    Json(json!({
        "request_id": Uuid::now_v7(),
        "account_id": Uuid::now_v7(),
        "message": {
            "id": "assistant-1",
            "model": {"provider": "openai", "id": "gpt-5.6-sol"},
            "duration_ms": 12,
            "native_message": {},
            "content": content,
            "stop_reason": stop_reason,
            "timestamp": 1
        }
    }))
    .into_response()
}

#[derive(Default)]
struct ExecutionState {
    operation: Mutex<Option<Value>>,
}

async fn machine(Path(machine_id): Path<String>) -> Json<Value> {
    assert_eq!(machine_id, "machine-a");
    Json(json!({
        "machine_id": "machine-a",
        "name": "Test machine",
        "connector": "machine_daemon",
        "online": true,
        "descriptor": {
            "protocol_version": {"major": 1, "minor": 0},
            "machine_id": "machine-a",
            "name": "Test machine",
            "operating_system": {"type": "linux"},
            "architecture": "x86_64",
            "path_convention": "posix",
            "workspace_roots": [{
                "id": "root",
                "name": "Workspace",
                "uri": "file:///workspace",
                "read_only": false
            }],
            "capabilities": [
                {"id": "workspace.query", "major": 1, "minor": 0},
                {"id": "workspace.mutation", "major": 1, "minor": 0}
            ]
        },
        "created_at": 1,
        "updated_at": 1,
        "last_seen_at": 1
    }))
}

async fn execute_operation(
    State(state): State<Arc<ExecutionState>>,
    Path(machine_id): Path<String>,
    Json(body): Json<Value>,
) -> Json<Value> {
    assert_eq!(machine_id, "machine-a");
    let operation = body["operation"].clone();
    *state.operation.lock().expect("operation") = Some(operation.clone());
    let mutation_id = operation["request"]["operation_id"].clone();
    Json(json!({
        "operation_id": "019d2aa0-0000-7000-8000-000000000001",
        "machine_id": "machine-a",
        "status": "completed",
        "operation": operation,
        "response": {
            "result": "mutation",
            "value": {
                "operation_id": mutation_id,
                "status": "committed",
                "changes": [],
                "atomic": true
            }
        },
        "created_at": 1,
        "updated_at": 1
    }))
}

async fn serve(app: Router) -> Url {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind test server");
    let address = listener.local_addr().expect("test server address");
    tokio::spawn(async move {
        axum::serve(listener, app).await.expect("serve test app");
    });
    format!("http://{address}").parse().expect("server URL")
}
