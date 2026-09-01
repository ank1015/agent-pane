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
use agent_harness_sdk::ActiveTurn;
use axum::{
    Json, Router,
    extract::{Path, State},
    http::StatusCode,
    response::{IntoResponse, Response},
    routing::{get, post},
};
use chrono::Utc;
use environment_harness::{
    clients::{AgentClient, ExecutionClient, LlmGatewayClient},
    config::{AgentServiceConfig, LlmGatewayServiceConfig},
    runtime::{EnvironmentRuntime, RetryPolicy, TurnOutcome},
};
use execution_gateway_client::ExecutionGatewayConfig;
use execution_runtime::OperationContext;
use llm_contracts::Message;
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
    let tools = requests[1]["request"]["tools"].as_array().unwrap();
    assert_eq!(tools.len(), 14);
    assert_eq!(tools[0]["name"], "get_tunnel_machines_list");
    assert_eq!(tools[0]["parameters"]["required"], json!([]));
    assert_eq!(tools[1]["name"], "get_sandbox_accounts_list");
    assert_eq!(tools[1]["parameters"]["required"], json!([]));
    assert_eq!(tools[2]["name"], "list_environments");
    assert_eq!(tools[2]["parameters"]["required"], json!([]));
    assert_eq!(tools[3]["name"], "create_sandbox");
    assert_eq!(tools[3]["parameters"]["required"], json!(["account_id"]));
    assert!(
        tools[3]["parameters"]["properties"]
            .get("snapshot_id")
            .is_some()
    );
    assert_eq!(tools[4]["name"], "snapshot_sandbox");
    assert_eq!(tools[4]["parameters"]["required"], json!(["machine_id"]));
    assert_eq!(tools[5]["name"], "create_tunnel_machine_environment");
    assert_eq!(
        tools[5]["parameters"]["required"],
        json!(["name", "machine_id", "path"])
    );
    assert_eq!(tools[6]["name"], "create_sandbox_template_environment");
    assert_eq!(
        tools[6]["parameters"]["required"],
        json!(["snapshot_id", "name", "path"])
    );
    assert!(
        tools[6]["parameters"]["properties"]
            .get("setup_script")
            .is_some()
    );
    assert_eq!(tools[7]["name"], "update_environment");
    assert_eq!(
        tools[7]["parameters"]["required"],
        json!(["environment_id"])
    );
    for tool in &tools[8..12] {
        assert!(
            tool["parameters"]["required"]
                .as_array()
                .expect("required tool fields")
                .contains(&json!("machineId"))
        );
    }
    assert_eq!(tools[12]["name"], "search");
    assert_eq!(tools[12]["parameters"]["required"], json!(["query"]));
    assert_eq!(tools[13]["name"], "scrape");
    assert_eq!(tools[13]["parameters"]["required"], json!(["url"]));
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
async fn omits_web_tools_and_guidance_when_disabled() {
    let agent = Arc::new(AgentState::new().with_web_search_enabled(false));
    let llm = Arc::new(LlmState::new(0));
    let runtime = runtime(&llm).await;
    let turn = active_turn(&agent).await;

    runtime.execute(&turn).await.expect("runtime outcome");

    let requests = llm.requests.lock().expect("LLM requests");
    let request = &requests[0]["request"];
    let tools = request["tools"].as_array().expect("tool definitions");
    assert_eq!(tools.len(), 12);
    assert!(tools.iter().all(|tool| tool["name"] != "search"));
    assert!(tools.iter().all(|tool| tool["name"] != "scrape"));
    assert!(
        !request["instructions"]
            .as_str()
            .expect("instructions")
            .contains("Use search to discover public web sources")
    );
}

#[tokio::test]
async fn sends_the_chatgpt_provider_profile_to_the_gateway() {
    let agent = Arc::new(AgentState::for_provider("chatgpt"));
    let llm = Arc::new(LlmState::new(0));
    let runtime = runtime(&llm).await;
    let turn = active_turn(&agent).await;

    let outcome = runtime.execute(&turn).await.expect("runtime outcome");

    assert!(matches!(
        outcome,
        TurnOutcome::Command(HarnessOperation::Complete { .. })
    ));
    let requests = llm.requests.lock().expect("LLM requests");
    let request = &requests[0]["request"];
    assert_eq!(request["model"]["provider"], "chatgpt");
    assert_eq!(
        request["provider_options"]["prompt_cache_key"],
        agent.session_id.to_string()
    );
    assert_eq!(
        request["provider_options"]["reasoning"],
        json!({"effort": "high", "summary": "auto"})
    );
    assert_eq!(
        request["provider_options"]["text"],
        json!({"verbosity": "low"})
    );
    assert_eq!(request["provider_options"]["tool_choice"], "auto");
    assert_eq!(request["provider_options"]["parallel_tool_calls"], true);
    assert!(
        request["provider_options"]
            .get("prompt_cache_options")
            .is_none()
    );
    assert!(
        request["provider_options"]
            .get("max_output_tokens")
            .is_none()
    );
}

#[tokio::test]
async fn sends_the_fireworks_provider_profile_to_the_gateway() {
    let agent = Arc::new(AgentState::for_provider("fireworks"));
    let llm = Arc::new(LlmState::new(0));
    let runtime = runtime(&llm).await;
    let turn = active_turn(&agent).await;

    let outcome = runtime.execute(&turn).await.expect("runtime outcome");

    assert!(matches!(
        outcome,
        TurnOutcome::Command(HarnessOperation::Complete { .. })
    ));
    let requests = llm.requests.lock().expect("LLM requests");
    let request = &requests[0]["request"];
    assert_eq!(request["model"]["provider"], "fireworks");
    assert_eq!(request["model"]["id"], "accounts/fireworks/models/kimi-k3");
    assert_eq!(
        request["provider_options"]["prompt_cache_key"],
        agent.session_id.to_string()
    );
    assert_eq!(request["provider_options"]["reasoning_effort"], "high");
    assert_eq!(request["provider_options"]["max_tokens"], 1_048_576);
    assert!(
        request["provider_options"]
            .get("reasoning_history")
            .is_none()
    );
    assert!(request["provider_options"].get("tool_choice").is_none());
    assert!(
        request["provider_options"]
            .get("parallel_tool_calls")
            .is_none()
    );
}

#[tokio::test]
async fn sends_the_deepseek_provider_profile_to_the_gateway() {
    let agent = Arc::new(AgentState::for_provider("deepseek"));
    let llm = Arc::new(LlmState::new(0));
    let runtime = runtime(&llm).await;
    let turn = active_turn(&agent).await;

    let outcome = runtime.execute(&turn).await.expect("runtime outcome");

    assert!(matches!(
        outcome,
        TurnOutcome::Command(HarnessOperation::Complete { .. })
    ));
    let requests = llm.requests.lock().expect("LLM requests");
    let request = &requests[0]["request"];
    assert_eq!(request["model"]["provider"], "deepseek");
    assert_eq!(request["model"]["id"], "deepseek-v4-pro");
    assert_eq!(
        request["provider_options"]["thinking"],
        json!({"type": "enabled"})
    );
    assert_eq!(request["provider_options"]["reasoning_effort"], "high");
    assert_eq!(request["provider_options"]["max_tokens"], 384_000);
    assert!(
        request["provider_options"]
            .get("prompt_cache_key")
            .is_none()
    );
    assert!(
        request["provider_options"]
            .get("prompt_cache_options")
            .is_none()
    );
    assert!(request["provider_options"].get("tool_choice").is_none());
    assert!(
        request["provider_options"]
            .get("parallel_tool_calls")
            .is_none()
    );
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

async fn runtime(llm: &Arc<LlmState>) -> EnvironmentRuntime {
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
    EnvironmentRuntime::new(
        llm_client,
        execution_client,
        tool_firecrawl_search::FirecrawlSearchToolContext::new("test-key").expect("search context"),
        tool_firecrawl_scrape::FirecrawlScrapeToolContext::new("test-key").expect("scrape context"),
    )
    .with_retry_policy(RetryPolicy {
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
    provider: &'static str,
    web_search_enabled: bool,
    revision: AtomicU64,
    appends: Mutex<Vec<AppendSessionMessages>>,
}

impl AgentState {
    fn new() -> Self {
        Self::for_provider("openai")
    }

    fn for_provider(provider: &'static str) -> Self {
        Self {
            run_id: Uuid::now_v7(),
            session_id: Uuid::now_v7(),
            trigger_session_message_id: Uuid::now_v7(),
            account_id: Uuid::now_v7(),
            provider,
            web_search_enabled: true,
            revision: AtomicU64::new(1),
            appends: Mutex::new(Vec::new()),
        }
    }

    fn with_web_search_enabled(mut self, enabled: bool) -> Self {
        self.web_search_enabled = enabled;
        self
    }

    fn request(&self) -> TurnRequested {
        let model_id = match self.provider {
            "fireworks" => "accounts/fireworks/models/kimi-k3",
            "deepseek" => "deepseek-v4-pro",
            _ => "gpt-5.6-sol",
        };
        TurnRequested {
            protocol_version: HARNESS_PROTOCOL_VERSION,
            event_id: Uuid::now_v7(),
            emitted_at: Utc::now(),
            run_id: self.run_id,
            session_id: self.session_id,
            turn_number: 1,
            max_turns: 100,
            expected_state_version: 7,
            harness_id: "environment".to_owned(),
            harness_slug: "environment".to_owned(),
            harness_revision_id: "environment-test".to_owned(),
            resolved_config: json!({
                "provider": self.provider,
                "model_id": model_id,
                "reasoning_level": "high",
                "account_id": self.account_id,
                "project_id": "019d2aa0-0000-7000-8000-000000000010",
                "web_search_enabled": self.web_search_enabled
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
    let provider = request["request"]["model"]["provider"]
        .as_str()
        .expect("request provider")
        .to_owned();
    let model_id = request["request"]["model"]["id"]
        .as_str()
        .expect("request model")
        .to_owned();
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
            "model": {"provider": provider, "id": model_id},
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
