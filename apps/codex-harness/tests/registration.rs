use std::{
    sync::{Arc, Mutex},
    time::Duration,
};

use agent_harness_sdk::AgentControlServiceConfig;
use axum::{
    Json, Router,
    extract::{OriginalUri, State},
    http::{HeaderMap, Method},
    routing::{patch, post, put},
};
use codex_harness::{CODEX_HARNESS_REVISION_ID, ensure_codex_harness};
use serde_json::{Value, json};
use url::Url;

#[tokio::test]
async fn registers_activates_and_enables_the_packaged_revision() {
    let calls = Arc::new(Mutex::new(Vec::new()));
    let app = Router::new()
        .route("/v1/harnesses", post(registry_call))
        .route("/v1/harnesses/{harness_id}", patch(registry_call))
        .route("/v1/harnesses/{harness_id}/revisions", post(registry_call))
        .route(
            "/v1/harnesses/{harness_id}/active-revision",
            put(registry_call),
        )
        .route("/v1/harnesses/{harness_id}/enabled", put(registry_call))
        .with_state(calls.clone());
    let config = AgentControlServiceConfig {
        base_url: serve(app).await,
        control_token: "control-secret".to_owned(),
        request_timeout: Duration::from_secs(2),
    };

    ensure_codex_harness(config)
        .await
        .expect("Codex registration");

    let calls = calls.lock().expect("registry calls");
    assert_eq!(calls.len(), 5);
    assert_eq!(calls[0].method, Method::POST);
    assert_eq!(calls[0].path, "/v1/harnesses");
    assert_eq!(calls[0].authorization, "Bearer control-secret");
    assert_eq!(calls[0].body["harness_id"], "codex");
    assert_eq!(calls[0].body["slug"], "codex");
    assert_eq!(
        calls[0].body["supported_providers"],
        json!([
            {
                "provider_id": "openai",
                "model_ids": ["gpt-5.6-sol", "gpt-5.6-terra", "gpt-5.6-luna"]
            },
            {
                "provider_id": "chatgpt",
                "model_ids": ["gpt-5.6-sol", "gpt-5.6-terra", "gpt-5.6-luna"]
            }
        ])
    );
    assert_eq!(calls[1].method, Method::PATCH);
    assert_eq!(calls[1].path, "/v1/harnesses/codex");
    assert_eq!(
        calls[2].body["harness_revision_id"],
        CODEX_HARNESS_REVISION_ID
    );
    assert_eq!(calls[2].body["default_config"]["provider"], "openai");
    assert_eq!(calls[2].body["default_config"]["model_id"], "gpt-5.6-sol");
    assert_eq!(
        calls[2].body["config_schema"]["properties"]["model_id"]["enum"],
        json!(["gpt-5.6-sol", "gpt-5.6-terra", "gpt-5.6-luna"])
    );
    assert_eq!(
        calls[2].body["config_schema"]["properties"]["execution"]["required"],
        json!(["machine_id", "workspace_root_id", "cwd"])
    );
    assert_eq!(calls[3].method, Method::PUT);
    assert_eq!(
        calls[3].body["harness_revision_id"],
        CODEX_HARNESS_REVISION_ID
    );
    assert_eq!(calls[4].body["enabled"], true);
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
        authorization: headers["authorization"]
            .to_str()
            .expect("authorization header")
            .to_owned(),
        body,
    });
    Json(json!({}))
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
