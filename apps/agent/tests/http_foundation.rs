use std::time::Duration;

use agent::{
    AppState, Database,
    auth::{ControlToken, HarnessToken},
    execution::ExecutionPolicy,
    router,
};
use reqwest::{Client, StatusCode, header::CACHE_CONTROL};
use serde_json::{Value, json};
use sqlx::postgres::PgPoolOptions;
use tokio::net::TcpListener;

const CONTROL_TOKEN: &str = "agent-control-token-that-is-long-enough";
const HARNESS_TOKEN: &str = "agent-harness-token-that-is-long-enough";

#[tokio::test]
async fn exposes_public_probes_and_json_routing_errors() {
    let pool = PgPoolOptions::new()
        .acquire_timeout(Duration::from_millis(50))
        .connect_lazy("postgres://postgres:postgres@127.0.0.1:1/agent")
        .expect("lazy test pool");
    let state = AppState::new(
        Database::from_pool(pool),
        ControlToken::new(CONTROL_TOKEN).expect("control token"),
        HarnessToken::new(HARNESS_TOKEN).expect("harness token"),
        ExecutionPolicy::default(),
    );
    let listener = TcpListener::bind("127.0.0.1:0")
        .await
        .expect("test listener");
    let address = listener.local_addr().expect("listener address");
    let server = tokio::spawn(async move {
        axum::serve(listener, router(state, 1024))
            .await
            .expect("test server");
    });
    let client = Client::new();
    let base_url = format!("http://{address}");

    let health = client
        .get(format!("{base_url}/health"))
        .send()
        .await
        .expect("health response");
    assert_eq!(health.status(), StatusCode::OK);
    assert_eq!(
        health.json::<Value>().await.expect("health body"),
        json!({ "status": "ok" })
    );

    let ready = client
        .get(format!("{base_url}/ready"))
        .send()
        .await
        .expect("readiness response");
    assert_eq!(ready.status(), StatusCode::SERVICE_UNAVAILABLE);
    assert_eq!(
        ready.json::<Value>().await.expect("readiness body"),
        json!({ "status": "unavailable" })
    );

    assert_error(
        client
            .get(format!("{base_url}/missing"))
            .send()
            .await
            .expect("not found response"),
        StatusCode::NOT_FOUND,
        "not_found",
    )
    .await;
    assert_error(
        client
            .post(format!("{base_url}/health"))
            .send()
            .await
            .expect("method not allowed response"),
        StatusCode::METHOD_NOT_ALLOWED,
        "method_not_allowed",
    )
    .await;

    server.abort();
}

async fn assert_error(response: reqwest::Response, status: StatusCode, code: &str) {
    assert_eq!(response.status(), status);
    assert_eq!(
        response
            .headers()
            .get(CACHE_CONTROL)
            .expect("cache control"),
        "no-store"
    );
    let body = response.json::<Value>().await.expect("error body");
    assert_eq!(body["error"]["code"], code);
    assert!(body["error"]["message"].is_string());
}
