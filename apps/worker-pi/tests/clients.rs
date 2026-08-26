use std::{
    collections::{BTreeMap, HashMap},
    sync::{Arc, Mutex},
    time::Duration,
};

use axum::{
    Json, Router,
    extract::{OriginalUri, Path, Query, State},
    http::{HeaderMap, Method},
    routing::get,
    routing::post,
};
use execution_gateway_client::ExecutionGatewayConfig;
use execution_runtime::OperationContext;
use llm_contracts::{LlmRequest, ModelId, ModelRef, ProviderId};
use serde_json::{Value, json};
use url::Url;
use uuid::Uuid;
use worker_pi::{
    clients::{
        AgentClient, ExecutionClient, HarnessRegistryClient, LlmGatewayClient,
        LlmGatewayClientError, PI_HARNESS_REVISION_ID,
    },
    config::{AgentControlServiceConfig, AgentServiceConfig, LlmGatewayServiceConfig},
};

#[tokio::test]
async fn completes_llm_requests_and_returns_the_assistant_message() {
    let captured = Arc::new(Mutex::new(None));
    let app = Router::new()
        .route("/v1/complete", post(complete))
        .with_state(captured.clone());
    let base_url = serve(app).await;
    let client = LlmGatewayClient::new(LlmGatewayServiceConfig {
        base_url,
        request_timeout: Duration::from_secs(5),
    })
    .expect("LLM client");
    let account_id = Uuid::now_v7();

    let message = client
        .complete(Some(account_id), &request(), &OperationContext::new())
        .await
        .expect("completion");

    assert_eq!(message.id.as_str(), "assistant-1");
    let captured = captured.lock().expect("capture lock").clone().unwrap();
    assert_eq!(captured["account_id"], account_id.to_string());
    assert_eq!(captured["request"]["model"]["provider"], "openai");
    assert_eq!(captured["request"]["model"]["id"], "gpt-5.6-sol");
}

#[tokio::test]
async fn cancels_an_llm_request_before_sending_it() {
    let client = LlmGatewayClient::new(LlmGatewayServiceConfig {
        base_url: "http://127.0.0.1:1".parse().expect("URL"),
        request_timeout: Duration::from_secs(5),
    })
    .expect("LLM client");
    let operation = OperationContext::new();
    operation.cancel();

    let error = client
        .complete(None, &request(), &operation)
        .await
        .expect_err("cancelled");

    assert!(matches!(error, LlmGatewayClientError::Cancelled));
}

#[tokio::test]
async fn fetches_every_agent_message_page_through_the_worker_route() {
    let calls = Arc::new(Mutex::new(Vec::new()));
    let app = Router::new()
        .route("/v1/worker/runs/{run_id}/messages", get(agent_messages))
        .with_state(calls.clone());
    let base_url = serve(app).await;
    let client = AgentClient::new(AgentServiceConfig {
        base_url,
        worker_token: "worker-token".to_owned(),
        request_timeout: Duration::from_secs(5),
    })
    .expect("Agent client");
    let run_id = Uuid::now_v7();

    let messages = client
        .fetch_session_messages(run_id, 7, "lease-token")
        .await
        .expect("session messages");

    assert_eq!(messages.len(), 2);
    let serialized = serde_json::to_value(messages).expect("messages serialize");
    assert_eq!(serialized[0]["message"]["id"], "user-1");
    assert_eq!(serialized[1]["message"]["id"], "user-2");
    let calls = calls.lock().expect("calls lock");
    assert_eq!(calls.len(), 2);
    assert_eq!(calls[0].run_id, run_id.to_string());
    assert_eq!(calls[0].authorization, "Bearer worker-token");
    assert_eq!(calls[0].lease_token, "lease-token");
    assert_eq!(calls[0].lease_version, "7");
    assert_eq!(calls[0].limit, "500");
    assert_eq!(calls[0].after_revision, None);
    assert_eq!(calls[1].after_revision.as_deref(), Some("1"));
}

#[test]
fn execution_client_uses_the_gateway_client_validation() {
    let config = ExecutionGatewayConfig::new("http://127.0.0.1:8790".parse().expect("URL"), "");

    assert!(ExecutionClient::new(config).is_err());
}

#[tokio::test]
async fn registers_and_activates_the_packaged_pi_harness() {
    let calls = Arc::new(Mutex::new(Vec::new()));
    let app = Router::new()
        .route("/v1/harnesses", post(registry_call))
        .route("/v1/harnesses/{harness_id}/revisions", post(registry_call))
        .route(
            "/v1/harnesses/{harness_id}/active-revision",
            axum::routing::put(registry_call),
        )
        .route(
            "/v1/harnesses/{harness_id}/enabled",
            axum::routing::put(registry_call),
        )
        .with_state(calls.clone());
    let client = HarnessRegistryClient::new(AgentControlServiceConfig {
        base_url: serve(app).await,
        control_token: "control-secret".to_owned(),
        request_timeout: Duration::from_secs(2),
    })
    .expect("registry client");

    client.ensure_pi_harness().await.expect("Pi registration");

    let calls = calls.lock().expect("registry calls");
    assert_eq!(calls.len(), 4);
    assert_eq!(calls[0].method, Method::POST);
    assert_eq!(calls[0].path, "/v1/harnesses");
    assert_eq!(calls[0].authorization, "Bearer control-secret");
    assert_eq!(calls[0].body["harness_id"], "pi");
    assert_eq!(calls[1].body["harness_revision_id"], PI_HARNESS_REVISION_ID);
    assert_eq!(calls[1].body["default_config"]["model_id"], "gpt-5.6-sol");
    assert_eq!(
        calls[1].body["config_schema"]["properties"]["execution"]["required"],
        json!(["machine_id", "workspace_root_id", "cwd"])
    );
    assert!(
        !calls[1].body["config_schema"]["required"]
            .as_array()
            .unwrap()
            .contains(&json!("execution"))
    );
    assert_eq!(calls[2].method, Method::PUT);
    assert_eq!(calls[2].body["harness_revision_id"], PI_HARNESS_REVISION_ID);
    assert_eq!(calls[3].body["enabled"], true);
}

async fn complete(
    State(captured): State<Arc<Mutex<Option<Value>>>>,
    Json(request): Json<Value>,
) -> Json<Value> {
    *captured.lock().expect("capture lock") = Some(request);
    Json(json!({
        "request_id": Uuid::now_v7(),
        "account_id": Uuid::now_v7(),
        "message": {
            "id": "assistant-1",
            "model": {
                "provider": "openai",
                "id": "gpt-5.6-sol"
            },
            "duration_ms": 12,
            "native_message": {},
            "content": [{
                "type": "response",
                "response": { "content": "done" }
            }],
            "stop_reason": "stop",
            "timestamp": 1
        }
    }))
}

#[derive(Debug)]
struct RegistryCall {
    method: Method,
    path: String,
    authorization: String,
    body: Value,
}

async fn registry_call(
    State(calls): State<Arc<Mutex<Vec<RegistryCall>>>>,
    method: Method,
    OriginalUri(uri): OriginalUri,
    headers: HeaderMap,
    Json(body): Json<Value>,
) -> Json<Value> {
    calls.lock().expect("registry calls").push(RegistryCall {
        method,
        path: uri.path().to_owned(),
        authorization: header(&headers, "authorization"),
        body,
    });
    Json(json!({}))
}

#[derive(Debug)]
struct AgentCall {
    run_id: String,
    authorization: String,
    lease_token: String,
    lease_version: String,
    limit: String,
    after_revision: Option<String>,
}

async fn agent_messages(
    State(calls): State<Arc<Mutex<Vec<AgentCall>>>>,
    Path(run_id): Path<String>,
    Query(query): Query<HashMap<String, String>>,
    headers: HeaderMap,
) -> Json<Value> {
    calls.lock().expect("calls lock").push(AgentCall {
        run_id,
        authorization: header(&headers, "authorization"),
        lease_token: header(&headers, "x-agent-lease-token"),
        lease_version: query["lease_version"].clone(),
        limit: query["limit"].clone(),
        after_revision: query.get("after_revision").cloned(),
    });

    match query.get("after_revision") {
        None => Json(message_page("user-1", "first", 1, Some(1))),
        Some(_) => Json(message_page("user-2", "second", 2, None)),
    }
}

fn message_page(id: &str, content: &str, revision: u64, next_after_revision: Option<u64>) -> Value {
    json!({
        "items": [{
            "session_message_id": Uuid::now_v7(),
            "session_id": Uuid::nil(),
            "revision": revision,
            "message": {
                "role": "user",
                "id": id,
                "timestamp": 1,
                "content": [{ "type": "text", "content": content }]
            },
            "origin": "external",
            "delivery": "immediate",
            "run_id": null,
            "turn_number": null,
            "created_at": "2026-08-25T00:00:00Z",
            "committed_at": "2026-08-25T00:00:00Z"
        }],
        "next_after_revision": next_after_revision
    })
}

fn header(headers: &HeaderMap, name: &str) -> String {
    headers[name].to_str().expect("ASCII header").to_owned()
}

fn request() -> LlmRequest {
    LlmRequest {
        model: ModelRef {
            provider: ProviderId::new("openai").expect("provider ID"),
            id: ModelId::new("gpt-5.6-sol").expect("model ID"),
            name: None,
        },
        instructions: None,
        messages: Vec::new(),
        tools: Vec::new(),
        provider_options: serde_json::Map::new(),
        metadata: BTreeMap::new(),
    }
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
