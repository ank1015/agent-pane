use std::{
    collections::BTreeMap,
    sync::{Arc, Mutex},
    time::Duration,
};

use axum::{Json, Router, extract::State, routing::post};
use codex_harness::{
    clients::{LlmGatewayClient, LlmGatewayClientError},
    config::LlmGatewayServiceConfig,
};
use execution_runtime::OperationContext;
use llm_contracts::{
    AllowedCaller, ExternalWebAccess, LlmRequest, ModelId, ModelRef, ProviderId, SearchCommands,
    SearchQuery, SearchRequest, SearchRequestOptions, SearchSettings,
};
use serde_json::{Map, Value, json};
use tokio::net::TcpListener;
use uuid::Uuid;

#[tokio::test]
async fn completes_a_non_streaming_gateway_request() {
    let captured = Arc::new(Mutex::new(None));
    let app = Router::new()
        .route("/v1/complete", post(complete))
        .with_state(captured.clone());
    let listener = TcpListener::bind("127.0.0.1:0").await.expect("bind server");
    let address = listener.local_addr().expect("server address");
    tokio::spawn(async move {
        axum::serve(listener, app).await.expect("serve gateway");
    });
    let client = LlmGatewayClient::new(LlmGatewayServiceConfig {
        base_url: format!("http://{address}").parse().expect("URL"),
        request_timeout: Duration::from_secs(5),
    })
    .expect("gateway client");
    let account_id = Uuid::now_v7();

    let message = client
        .complete(Some(account_id), &request(), &OperationContext::new())
        .await
        .expect("completion");

    assert_eq!(message.id.as_str(), "assistant-1");
    let captured = captured.lock().expect("capture lock").clone().unwrap();
    assert_eq!(captured["account_id"], json!(account_id));
    assert_eq!(captured["request"]["model"]["provider"], "openai");
    assert_eq!(captured["request"]["model"]["id"], "gpt-5.6-sol");
}

#[tokio::test]
async fn cancellation_prevents_a_gateway_request() {
    let client = LlmGatewayClient::new(LlmGatewayServiceConfig {
        base_url: "http://127.0.0.1:1".parse().expect("URL"),
        request_timeout: Duration::from_secs(5),
    })
    .expect("gateway client");
    let operation = OperationContext::new();
    operation.cancel();

    let error = client
        .complete(None, &request(), &operation)
        .await
        .expect_err("cancelled");

    assert!(matches!(error, LlmGatewayClientError::Cancelled));
}

#[tokio::test]
async fn executes_a_standalone_search_gateway_request() {
    let captured = Arc::new(Mutex::new(None));
    let app = Router::new()
        .route("/v1/search", post(search))
        .with_state(captured.clone());
    let listener = TcpListener::bind("127.0.0.1:0").await.expect("bind server");
    let address = listener.local_addr().expect("server address");
    tokio::spawn(async move {
        axum::serve(listener, app).await.expect("serve gateway");
    });
    let client = LlmGatewayClient::new(LlmGatewayServiceConfig {
        base_url: format!("http://{address}").parse().expect("URL"),
        request_timeout: Duration::from_secs(5),
    })
    .expect("gateway client");
    let account_id = Uuid::now_v7();
    let provider = ProviderId::new("chatgpt").expect("provider");
    let request = SearchRequest {
        id: "session-1".to_owned(),
        model: "gpt-5.6-sol".to_owned(),
        reasoning: None,
        input: None,
        commands: Some(SearchCommands {
            search_query: Some(vec![SearchQuery {
                q: "Codex web.run".to_owned(),
                recency: None,
                domains: None,
            }]),
            ..Default::default()
        }),
        settings: Some(SearchSettings {
            allowed_callers: Some(vec![AllowedCaller::Direct]),
            external_web_access: Some(ExternalWebAccess::Boolean(false)),
            ..Default::default()
        }),
        max_output_tokens: Some(2_500),
    };

    let response = client
        .search(
            Some(account_id),
            &provider,
            &request,
            &SearchRequestOptions::default(),
            &OperationContext::new(),
        )
        .await
        .expect("search response");

    assert_eq!(response.output, "search output");
    let captured = captured.lock().expect("capture lock").clone().unwrap();
    assert_eq!(captured["account_id"], account_id.to_string());
    assert_eq!(captured["provider"], "chatgpt");
    assert_eq!(captured["request"]["id"], "session-1");
    assert_eq!(
        captured["request"]["commands"]["search_query"][0]["q"],
        "Codex web.run"
    );
    assert_eq!(
        captured["request"]["settings"]["allowed_callers"],
        json!(["direct"])
    );
    assert_eq!(
        captured["request"]["settings"]["external_web_access"],
        false
    );
    assert_eq!(captured["request"]["max_output_tokens"], 2_500);
    assert_eq!(captured["request_options"], json!({}));
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
            "native_message": {"output": []},
            "content": [{
                "type": "response",
                "response": {"content": "done"}
            }],
            "stop_reason": "stop",
            "timestamp": 1
        }
    }))
}

async fn search(
    State(captured): State<Arc<Mutex<Option<Value>>>>,
    Json(request): Json<Value>,
) -> Json<Value> {
    *captured.lock().expect("capture lock") = Some(request);
    Json(json!({
        "request_id": Uuid::now_v7(),
        "account_id": Uuid::now_v7(),
        "response": {
            "encrypted_output": null,
            "output": "search output",
            "results": [{"type": "computer_initialize_state"}]
        }
    }))
}

fn request() -> LlmRequest {
    LlmRequest {
        model: ModelRef {
            provider: ProviderId::new("openai").expect("provider"),
            id: ModelId::new("gpt-5.6-sol").expect("model"),
            name: None,
        },
        instructions: None,
        messages: Vec::new(),
        tools: Vec::new(),
        provider_options: Map::new(),
        metadata: BTreeMap::new(),
    }
}
