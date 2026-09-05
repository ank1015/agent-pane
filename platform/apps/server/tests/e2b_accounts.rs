use axum::{
    Json, Router,
    http::{HeaderMap, StatusCode},
    routing::get,
};
use platform_server::{machines::ExecutionGatewayClient, providers::LlmGatewayClient, router};
use serde_json::{Value, json};
use std::{
    sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    },
    time::Duration,
};
use tokio::{net::TcpListener, task::JoinHandle};

const PATH: &str = "/api/machines/e2b-accounts";

fn account() -> Value {
    json!({"id":"018f47a8-80cc-7b2f-9d44-6657f5f82ad0","name":"Test account",
        "credential_fingerprint":"test-fingerprint","is_default":false,"status":"active",
        "last_verified_at":null,"created_at":"2026-09-03T00:00:00Z","updated_at":"2026-09-03T00:00:00Z"})
}

struct TestServer {
    url: String,
    task: JoinHandle<()>,
}
impl Drop for TestServer {
    fn drop(&mut self) {
        self.task.abort();
    }
}
async fn serve(app: Router) -> TestServer {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}", listener.local_addr().unwrap());
    let task = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    TestServer { url, task }
}
async fn platform(gateway: &TestServer) -> TestServer {
    serve(router(
        ExecutionGatewayClient::new(
            gateway.url.parse().unwrap(),
            "test-token",
            Duration::from_secs(2),
        )
        .unwrap(),
        LlmGatewayClient::new(
            gateway.url.parse().unwrap(),
            "test-llm-token",
            Duration::from_secs(2),
        )
        .unwrap(),
    ))
    .await
}

#[tokio::test]
async fn lists_and_creates_accounts_with_bearer_auth_and_no_key_in_response() {
    let calls = Arc::new(AtomicUsize::new(0));
    let counter = calls.clone();
    let gateway = serve(
        Router::new().route(
            "/v1/e2b-accounts",
            get(|headers: HeaderMap| async move {
                assert_eq!(headers["authorization"], "Bearer test-token");
                Json(vec![account()])
            })
            .post(
                move |headers: HeaderMap, Json(body): Json<Value>| async move {
                    counter.fetch_add(1, Ordering::SeqCst);
                    assert_eq!(headers["authorization"], "Bearer test-token");
                    assert_eq!(
                        body,
                        json!({"name":"Test account","api_key":"test-e2b-key"})
                    );
                    (StatusCode::CREATED, Json(account()))
                },
            ),
        ),
    )
    .await;
    let app = platform(&gateway).await;
    let client = reqwest::Client::new();
    let list = client
        .get(format!("{}{PATH}", app.url))
        .send()
        .await
        .unwrap();
    assert_eq!(list.status(), StatusCode::OK);
    assert_eq!(list.headers()["cache-control"], "no-store");
    assert_eq!(list.json::<Value>().await.unwrap(), json!([account()]));
    let created = client
        .post(format!("{}{PATH}", app.url))
        .json(&json!({"name":"Test account","api_key":"test-e2b-key"}))
        .send()
        .await
        .unwrap();
    assert_eq!(created.status(), StatusCode::CREATED);
    assert_eq!(created.headers()["cache-control"], "no-store");
    assert_eq!(created.json::<Value>().await.unwrap(), account());
    assert_eq!(calls.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn invalid_input_is_rejected_locally_without_echoing_credentials() {
    let calls = Arc::new(AtomicUsize::new(0));
    let counter = calls.clone();
    let gateway = serve(Router::new().fallback(move || async move {
        counter.fetch_add(1, Ordering::SeqCst);
        StatusCode::INTERNAL_SERVER_ERROR
    }))
    .await;
    let app = platform(&gateway).await;
    let client = reqwest::Client::new();
    for body in [
        json!({"name":" ","api_key":"secret-marker"}),
        json!({"name":"ok","api_key":""}),
        json!({"name":"ok","api_key":" secret-marker"}),
        json!({"name":"a".repeat(201),"api_key":"secret-marker"}),
        json!({"name":"ok","api_key":"x".repeat(4097)}),
        json!({"name":"ok","api_key":"secret-marker","extra":true}),
        json!({"name":"ok"}),
        json!({"name":"ok","api_key":123}),
        json!({"name":"ok","api_key":"x".repeat(17000)}),
    ] {
        let result = client
            .post(format!("{}{PATH}", app.url))
            .json(&body)
            .send()
            .await
            .unwrap();
        assert!(result.status().is_client_error());
        assert_eq!(result.headers()["cache-control"], "no-store");
        assert!(!result.text().await.unwrap().contains("secret-marker"));
    }
    let malformed = client
        .post(format!("{}{PATH}", app.url))
        .header("content-type", "application/json")
        .body("{secret-marker")
        .send()
        .await
        .unwrap();
    assert_eq!(malformed.status(), StatusCode::BAD_REQUEST);
    assert!(!malformed.text().await.unwrap().contains("secret-marker"));
    assert_eq!(calls.load(Ordering::SeqCst), 0);
}

#[tokio::test]
async fn upstream_failures_are_sanitized_and_never_retried() {
    for (upstream, expected) in [
        (422, 422),
        (400, 400),
        (409, 409),
        (429, 429),
        (401, 502),
        (403, 502),
        (500, 502),
        (307, 502),
    ] {
        let calls = Arc::new(AtomicUsize::new(0));
        let counter = calls.clone();
        let gateway = serve(Router::new().fallback(move || async move {
            counter.fetch_add(1, Ordering::SeqCst);
            (
                StatusCode::from_u16(upstream).unwrap(),
                [("location", "/redirected")],
                "upstream-secret-marker",
            )
        }))
        .await;
        let app = platform(&gateway).await;
        let response = reqwest::Client::new()
            .post(format!("{}{PATH}", app.url))
            .json(&json!({"name":"Test account","api_key":"test-e2b-key"}))
            .send()
            .await
            .unwrap();
        assert_eq!(response.status().as_u16(), expected);
        assert!(
            !response
                .text()
                .await
                .unwrap()
                .contains("upstream-secret-marker")
        );
        assert_eq!(calls.load(Ordering::SeqCst), 1);
    }
}

#[tokio::test]
async fn timeout_warns_to_check_accounts_and_is_not_retried() {
    let calls = Arc::new(AtomicUsize::new(0));
    let counter = calls.clone();
    let gateway = serve(Router::new().fallback(move || async move {
        counter.fetch_add(1, Ordering::SeqCst);
        tokio::time::sleep(Duration::from_secs(1)).await;
        (StatusCode::CREATED, Json(account()))
    }))
    .await;
    let app = serve(router(
        ExecutionGatewayClient::new(
            gateway.url.parse().unwrap(),
            "test-token",
            Duration::from_millis(100),
        )
        .unwrap(),
        LlmGatewayClient::new(
            gateway.url.parse().unwrap(),
            "test-llm-token",
            Duration::from_secs(2),
        )
        .unwrap(),
    ))
    .await;
    let response = reqwest::Client::new()
        .post(format!("{}{PATH}", app.url))
        .json(&json!({"name":"Test account","api_key":"test-e2b-key"}))
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::GATEWAY_TIMEOUT);
    assert!(
        response
            .text()
            .await
            .unwrap()
            .contains("Refresh the account list")
    );
    assert_eq!(calls.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn malformed_success_is_not_forwarded() {
    let gateway =
        serve(Router::new().fallback(|| async { (StatusCode::CREATED, "upstream-secret-marker") }))
            .await;
    let app = platform(&gateway).await;
    let response = reqwest::Client::new()
        .post(format!("{}{PATH}", app.url))
        .json(&json!({"name":"Test account","api_key":"test-e2b-key"}))
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::BAD_GATEWAY);
    assert!(
        !response
            .text()
            .await
            .unwrap()
            .contains("upstream-secret-marker")
    );
}
