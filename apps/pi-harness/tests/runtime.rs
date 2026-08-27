use std::{
    sync::{
        Arc, Mutex,
        atomic::{AtomicU64, AtomicUsize, Ordering},
    },
    time::Duration,
};

use agent_contracts::{
    AppendSessionMessages, HARNESS_PROTOCOL_VERSION, HarnessOperation, SessionMessage,
    SessionMessageDelivery, SessionMessageOrigin, SessionMessagePage, SessionMessagesAppended,
    TurnRequested,
};
use axum::{
    Json, Router,
    extract::{Path, State},
    http::StatusCode,
    response::{IntoResponse, Response},
    routing::{get, post},
};
use chrono::Utc;
use execution_gateway_client::ExecutionGatewayConfig;
use execution_runtime::OperationContext;
use llm_contracts::Message;
use pi_harness::{
    clients::{AgentClient, ExecutionClient, LlmGatewayClient},
    config::{AgentServiceConfig, LlmGatewayServiceConfig},
    runtime::{PiRuntime, RetryPolicy, TurnOutcome},
    server::ActiveTurn,
};
use serde_json::{Value, json};
use url::Url;
use uuid::Uuid;

#[tokio::test]
async fn retries_a_transient_llm_failure_inside_the_harness_and_completes() {
    let agent = Arc::new(AgentState::new());
    let llm = Arc::new(LlmState::new(1));
    let runtime = runtime(&llm).await;
    let turn = active_turn(&agent).await;

    let outcome = runtime.execute(&turn).await.expect("runtime outcome");

    let TurnOutcome::Command(HarnessOperation::Complete { final_message_id }) = outcome else {
        panic!("turn should complete");
    };
    assert_eq!(llm.calls.load(Ordering::Acquire), 2);
    let requests = llm.requests.lock().expect("LLM requests");
    assert_eq!(requests.len(), 2);
    assert_eq!(requests[1]["account_id"], agent.account_id.to_string());
    assert_eq!(
        requests[1]["request"]["messages"].as_array().unwrap().len(),
        1
    );
    assert_eq!(requests[1]["request"]["tools"].as_array().unwrap().len(), 4);
    drop(requests);

    let appends = agent.appends.lock().expect("Agent appends");
    assert_eq!(appends.len(), 1);
    assert_eq!(appends[0].expected_state_version, 7);
    assert_eq!(appends[0].turn_number, 1);
    assert_eq!(appends[0].expected_session_revision, 1);
    assert_eq!(appends[0].messages[0].session_message_id, final_message_id);
    assert!(matches!(
        appends[0].messages[0].message,
        Message::Assistant(_)
    ));
}

#[tokio::test]
async fn converts_exhausted_retryable_llm_failures_into_an_expiring_wait() {
    let agent = Arc::new(AgentState::new());
    let llm = Arc::new(LlmState::new(usize::MAX));
    let runtime = runtime(&llm).await;
    let turn = active_turn(&agent).await;

    let outcome = runtime.execute(&turn).await.expect("runtime outcome");

    let TurnOutcome::Command(HarnessOperation::Wait(wait)) = outcome else {
        panic!("retry exhaustion should suspend the run");
    };
    assert_eq!(llm.calls.load(Ordering::Acquire), 2);
    assert_eq!(wait.wait_id, turn.request().event_id);
    assert_eq!(wait.kind, "llm_retry");
    assert!(wait.harness_wait_id.starts_with("llm-backoff-"));
    assert!(wait.expires_at.is_some_and(|expiry| expiry > Utc::now()));
    assert_eq!(wait.public_request["code"], "llm_request_failed");
    assert!(agent.appends.lock().expect("Agent appends").is_empty());
}

async fn runtime(llm: &Arc<LlmState>) -> PiRuntime {
    let llm_app = Router::new()
        .route("/v1/complete", post(llm_complete))
        .with_state(llm.clone());
    let llm_client = LlmGatewayClient::new(LlmGatewayServiceConfig {
        base_url: serve(llm_app).await,
        request_timeout: Duration::from_secs(2),
    })
    .expect("LLM client");
    let execution_client = ExecutionClient::new(ExecutionGatewayConfig::new(
        "http://127.0.0.1:1".parse().expect("execution URL"),
        "execution-secret",
    ))
    .expect("execution client");
    PiRuntime::new(llm_client, execution_client).with_retry_policy(RetryPolicy {
        max_retries: 1,
        base_delay: Duration::from_millis(1),
        max_retry_delay: Duration::from_millis(5),
    })
}

async fn active_turn(state: &Arc<AgentState>) -> ActiveTurn {
    let agent_app = Router::new()
        .route(
            "/v1/harness/runs/{run_id}/messages",
            get(messages).post(append_messages),
        )
        .with_state(state.clone());
    let client = AgentClient::new(AgentServiceConfig {
        base_url: serve(agent_app).await,
        harness_token: "harness-secret".to_owned(),
        request_timeout: Duration::from_secs(2),
    })
    .expect("Agent client");
    ActiveTurn::new(client, state.request(), OperationContext::new())
}

struct AgentState {
    run_id: Uuid,
    session_id: Uuid,
    trigger_session_message_id: Uuid,
    account_id: Uuid,
    revision: AtomicU64,
    appends: Mutex<Vec<AppendSessionMessages>>,
}

impl AgentState {
    fn new() -> Self {
        Self {
            run_id: Uuid::now_v7(),
            session_id: Uuid::now_v7(),
            trigger_session_message_id: Uuid::now_v7(),
            account_id: Uuid::now_v7(),
            revision: AtomicU64::new(1),
            appends: Mutex::new(Vec::new()),
        }
    }

    fn request(&self) -> TurnRequested {
        TurnRequested {
            protocol_version: HARNESS_PROTOCOL_VERSION,
            event_id: Uuid::now_v7(),
            emitted_at: Utc::now(),
            run_id: self.run_id,
            session_id: self.session_id,
            turn_number: 1,
            max_turns: 100,
            expected_state_version: 7,
            harness_id: "pi".to_owned(),
            harness_slug: "pi".to_owned(),
            harness_revision_id: "pi-test".to_owned(),
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
            current_session_revision: self.revision.load(Ordering::Acquire),
            resume: None,
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
    Json(command): Json<AppendSessionMessages>,
) -> Json<SessionMessagesAppended> {
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
    state.appends.lock().expect("Agent appends").push(command);
    Json(SessionMessagesAppended {
        items,
        current_session_revision: revision,
    })
}

struct LlmState {
    calls: AtomicUsize,
    failures_before_success: usize,
    requests: Mutex<Vec<Value>>,
}

impl LlmState {
    fn new(failures_before_success: usize) -> Self {
        Self {
            calls: AtomicUsize::new(0),
            failures_before_success,
            requests: Mutex::new(Vec::new()),
        }
    }
}

async fn llm_complete(State(state): State<Arc<LlmState>>, Json(request): Json<Value>) -> Response {
    state.requests.lock().expect("LLM requests").push(request);
    let call = state.calls.fetch_add(1, Ordering::AcqRel);
    if call < state.failures_before_success {
        return (
            StatusCode::TOO_MANY_REQUESTS,
            Json(json!({
                "error": {
                    "kind": "provider",
                    "message": "provider rate limit",
                    "can_retry": true,
                    "retry_after_ms": 0,
                    "provider_error": null
                }
            })),
        )
            .into_response();
    }

    Json(json!({
        "request_id": Uuid::now_v7(),
        "account_id": Uuid::now_v7(),
        "message": {
            "id": "assistant-1",
            "model": {"provider": "openai", "id": "gpt-5.6-sol"},
            "duration_ms": 12,
            "native_message": {},
            "content": [{
                "type": "response",
                "response": {"content": "The project is ready."}
            }],
            "stop_reason": "stop",
            "timestamp": 1
        }
    }))
    .into_response()
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
