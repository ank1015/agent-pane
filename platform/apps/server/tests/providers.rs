use std::{
    sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    },
    time::Duration,
};

use axum::{
    Json, Router,
    http::{HeaderMap, StatusCode, Uri},
    routing::{get, post},
};
use platform_server::{machines::ExecutionGatewayClient, providers::LlmGatewayClient, router};
use serde_json::{Value, json};
use tokio::{net::TcpListener, task::JoinHandle};

const PATH: &str = "/api/providers";
const GATEWAY_PATH: &str = "/gateway/v1/admin/accounts";

const ANALYTICS_ID: &str = "018f47a8-80cc-7b2f-9d44-6657f5f82a01";

#[tokio::test]
async fn analytics_totals_preserve_all_breakdowns_and_drop_extra_fields() {
    let gateway = serve(Router::new().route(
        &format!("{GATEWAY_PATH}/{ANALYTICS_ID}/usage"),
        get(|headers: HeaderMap, uri: Uri| async move {
            assert_eq!(headers["authorization"], "Bearer admin-only-token");
            assert!(
                uri.query().is_none(),
                "all-time totals must not be filtered"
            );
            Json(json!({"totals":{
                "request_count":4,"succeeded_count":3,"failed_count":1,"usage_record_count":3,
                "tokens":{"input":100,"output":25,"cache_read":50,"cache_write":10},
                "costs":{"total":0.42,"input":0.2,"output":0.1,"cache_read":0.1,"cache_write":0.02},
                "secret":"secret-marker"
            },"groups":[]}))
        }),
    ))
    .await;
    let app = platform(&gateway, Duration::from_secs(2)).await;
    let response = reqwest::get(format!("{}{PATH}/{ANALYTICS_ID}/usage", app.url))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(response.headers()["cache-control"], "no-store");
    let text = response.text().await.unwrap();
    assert!(!text.contains("secret-marker"));
    let body: Value = serde_json::from_str(&text).unwrap();
    assert_eq!(body["account_id"], ANALYTICS_ID);
    assert_eq!(body["request_count"], 4);
    assert_eq!(body["tokens"]["cache_read"], 50);
    assert_eq!(body["costs"]["total"], 0.42);
}

#[tokio::test]
async fn analytics_requests_forward_cursor_and_keep_missing_usage_null() {
    let gateway = serve(Router::new().route(
        &format!("{GATEWAY_PATH}/{ANALYTICS_ID}/requests"),
        get(|headers: HeaderMap, uri: Uri| async move {
            assert_eq!(headers["authorization"], "Bearer admin-only-token");
            let url = url::Url::parse(&format!("http://localhost{uri}")).unwrap();
            let query: std::collections::HashMap<_, _> = url.query_pairs().into_owned().collect();
            assert_eq!(query["limit"], "25");
            assert_eq!(query["cursor"], "opaque+/cursor=");
            Json(json!({"items":[{
                "id":ANALYTICS_ID,"account_id":ANALYTICS_ID,"provider":"openai",
                "requested_model":"gpt-test","response_model":null,"status":"running",
                "started_at":"2026-09-04T00:00:00Z","completed_at":null,"usage":null,
                "labels":{"secret":"secret-marker"},"provider_error_code":"secret-marker"
            }],"next_cursor":"next-page"}))
        }),
    ))
    .await;
    let app = platform(&gateway, Duration::from_secs(2)).await;
    let response = reqwest::Client::new()
        .get(format!("{}{PATH}/{ANALYTICS_ID}/requests", app.url))
        .query(&[("limit", "25"), ("cursor", "opaque+/cursor=")])
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(response.headers()["cache-control"], "no-store");
    let text = response.text().await.unwrap();
    assert!(!text.contains("secret-marker"));
    let body: Value = serde_json::from_str(&text).unwrap();
    assert_eq!(body["next_cursor"], "next-page");
    assert!(body["items"][0]["usage"].is_null());
    assert!(body["items"][0]["completed_at"].is_null());
}

#[tokio::test]
async fn analytics_reject_invalid_queries_before_gateway() {
    let calls = Arc::new(AtomicUsize::new(0));
    let counter = calls.clone();
    let gateway = serve(Router::new().fallback(move || async move {
        counter.fetch_add(1, Ordering::SeqCst);
        StatusCode::INTERNAL_SERVER_ERROR
    }))
    .await;
    let app = platform(&gateway, Duration::from_secs(2)).await;
    for suffix in [
        "limit=0",
        "limit=101",
        "limit=abc",
        "cursor=",
        "account_id=other",
    ] {
        let response = reqwest::get(format!(
            "{}{PATH}/{ANALYTICS_ID}/requests?{suffix}",
            app.url
        ))
        .await
        .unwrap();
        assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    }
    let response = reqwest::get(format!("{}{PATH}/invalid/usage", app.url))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    assert_eq!(calls.load(Ordering::SeqCst), 0);
}

#[tokio::test]
async fn analytics_errors_are_sanitized_and_bad_contracts_are_rejected() {
    for (upstream, body, expected) in [
        (404, "secret-marker", 404),
        (401, "secret-marker", 502),
        (429, "secret-marker", 503),
        (504, "secret-marker", 504),
        (400, "secret-marker", 400),
        (200, "{}", 502),
        (200, "secret-marker", 502),
        (302, "secret-marker", 502),
    ] {
        let gateway = serve(
            Router::new()
                .fallback(move || async move { (StatusCode::from_u16(upstream).unwrap(), body) }),
        )
        .await;
        let app = platform(&gateway, Duration::from_secs(2)).await;
        for resource in ["usage", "requests"] {
            let response = reqwest::get(format!("{}{PATH}/{ANALYTICS_ID}/{resource}", app.url))
                .await
                .unwrap();
            assert_eq!(response.status().as_u16(), expected);
            assert_eq!(response.headers()["cache-control"], "no-store");
            assert!(!response.text().await.unwrap().contains("secret-marker"));
        }
    }
}

struct TestServer {
    url: String,
    task: JoinHandle<()>,
}

#[tokio::test]
async fn creates_api_key_account_with_exact_gateway_contract_and_no_secret_response() {
    let calls = Arc::new(AtomicUsize::new(0));
    let counter = calls.clone();
    let gateway = serve(Router::new().route(
        GATEWAY_PATH,
        post(move |headers: HeaderMap, Json(body): Json<Value>| async move {
            counter.fetch_add(1, Ordering::SeqCst);
            assert_eq!(headers["authorization"], "Bearer admin-only-token");
            assert_eq!(body, json!({
                "provider":"fireworks", "name":"Team Fireworks",
                "credentials":{"api_key":"fw-secret-marker"}
            }));
            (StatusCode::CREATED, Json(json!({"account": account(9, "fireworks", "active", "2026-09-04T00:00:00Z")})))
        }),
    )).await;
    let app = platform(&gateway, Duration::from_secs(2)).await;
    let response = reqwest::Client::new()
        .post(format!("{}{PATH}", app.url))
        .json(&json!({"provider":"fireworks","name":"Team Fireworks","api_key":"fw-secret-marker"}))
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::CREATED);
    assert_eq!(response.headers()["cache-control"], "no-store");
    let body = response.text().await.unwrap();
    assert!(!body.contains("secret-marker"));
    assert!(!body.contains("credentials"));
    assert_eq!(
        serde_json::from_str::<Value>(&body).unwrap()["provider"],
        "fireworks"
    );
    assert_eq!(calls.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn rejects_invalid_and_chatgpt_api_key_creation_without_calling_gateway() {
    let calls = Arc::new(AtomicUsize::new(0));
    let counter = calls.clone();
    let gateway = serve(Router::new().fallback(move || async move {
        counter.fetch_add(1, Ordering::SeqCst);
        StatusCode::INTERNAL_SERVER_ERROR
    }))
    .await;
    let app = platform(&gateway, Duration::from_secs(2)).await;
    for input in [
        json!({"provider":"openai","name":"","api_key":"secret-marker"}),
        json!({"provider":"openai","name":"Personal","api_key":" secret-marker"}),
        json!({"provider":"chatgpt","name":"Personal","api_key":"secret-marker"}),
        json!({"provider":"unknown","name":"Personal","api_key":"secret-marker"}),
        json!({"provider":"openai","name":"Personal","api_key":"secret-marker","extra":true}),
    ] {
        let response = reqwest::Client::new()
            .post(format!("{}{PATH}", app.url))
            .json(&input)
            .send()
            .await
            .unwrap();
        assert!(matches!(
            response.status(),
            StatusCode::BAD_REQUEST | StatusCode::UNPROCESSABLE_ENTITY
        ));
        assert!(!response.text().await.unwrap().contains("secret-marker"));
    }
    assert_eq!(calls.load(Ordering::SeqCst), 0);
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

async fn platform(gateway: &TestServer, timeout: Duration) -> TestServer {
    serve(router(
        ExecutionGatewayClient::new(
            gateway.url.parse().unwrap(),
            "execution-only-token",
            timeout,
        )
        .unwrap(),
        LlmGatewayClient::new(
            format!("{}/gateway", gateway.url).parse().unwrap(),
            "admin-only-token",
            timeout,
        )
        .unwrap(),
    ))
    .await
}

fn account(id: u8, provider: &str, status: &str, created_at: &str) -> Value {
    json!({
        "id":format!("018f47a8-80cc-7b2f-9d44-6657f5f82a{id:02}"),
        "provider":provider,"name":format!("Account {id}"),"status":status,
        "is_default":id == 1,"created_at":created_at,"updated_at":created_at,
        "runtime_revision":1,"config":{"internal":"not-for-dashboard"},
        "credential":{"version":1,"encryption_key_version":1,"updated_at":created_at},
        "unexpected_secret":"secret-marker-never-forward"
    })
}

#[tokio::test]
async fn lists_all_accounts_with_admin_auth_and_dashboard_summary_only() {
    let calls = Arc::new(AtomicUsize::new(0));
    let counter = calls.clone();
    let gateway = serve(Router::new().route(
        GATEWAY_PATH,
        get(move |headers: HeaderMap, uri: Uri| async move {
            counter.fetch_add(1, Ordering::SeqCst);
            assert_eq!(headers["authorization"], "Bearer admin-only-token");
            assert_eq!(headers["accept"], "application/json");
            assert!(uri.query().is_none(), "must not filter out any accounts");
            Json(json!({"accounts":[
                account(4,"openai","active","2026-09-03T00:00:00Z"),
                account(3,"fireworks","disabled","2026-09-03T00:00:00Z"),
                account(2,"chatgpt","reauth_required","2026-09-03T00:00:00Z"),
                account(1,"openai","active","2026-09-04T00:00:00Z"),
                account(5,"openai","active","2026-09-03T00:00:00Z")
            ]}))
        }),
    ))
    .await;
    let app = platform(&gateway, Duration::from_secs(2)).await;
    let response = reqwest::Client::new()
        .get(format!("{}{PATH}", app.url))
        .bearer_auth("browser-token-must-not-be-forwarded")
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(response.headers()["cache-control"], "no-store");
    let body = response.text().await.unwrap();
    assert!(!body.contains("secret-marker"));
    assert!(!body.contains("credential"));
    assert!(!body.contains("config"));
    let rows: Vec<Value> = serde_json::from_str(&body).unwrap();
    assert_eq!(
        rows.iter()
            .map(|row| row["name"].as_str().unwrap())
            .collect::<Vec<_>>(),
        [
            "Account 2",
            "Account 3",
            "Account 1",
            "Account 4",
            "Account 5"
        ]
    );
    assert_eq!(rows[0]["status"], "reauth_required");
    assert_eq!(rows[1]["status"], "disabled");
    assert_eq!(rows[2]["status"], "enabled");
    assert_eq!(rows[2]["is_default"], true);
    assert_eq!(rows[2]["created_at"], "2026-09-04T00:00:00Z");
    assert!(rows.iter().all(|row| row.as_object().unwrap().len() == 6));
    assert_eq!(calls.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn gets_one_provider_detail_with_safe_gateway_metadata() {
    let id = "018f47a8-80cc-7b2f-9d44-6657f5f82a07";
    let gateway_path = format!("{GATEWAY_PATH}/{id}");
    let gateway = serve(Router::new().route(
        &gateway_path,
        get(move |headers: HeaderMap, uri: Uri| async move {
            assert_eq!(headers["authorization"], "Bearer admin-only-token");
            assert_eq!(headers["accept"], "application/json");
            assert!(uri.query().is_none());
            Json(json!({"account": account(7, "chatgpt", "reauth_required", "2026-09-04T00:00:00Z")}))
        }),
    )).await;
    let app = platform(&gateway, Duration::from_secs(2)).await;
    let response = reqwest::get(format!("{}{PATH}/{id}", app.url))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(response.headers()["cache-control"], "no-store");
    let body = response.text().await.unwrap();
    assert!(!body.contains("secret-marker"));
    let provider = &serde_json::from_str::<Value>(&body).unwrap()["provider"];
    assert_eq!(provider["id"], id);
    assert_eq!(provider["provider"], "chatgpt");
    assert_eq!(provider["status"], "reauth_required");
    assert_eq!(provider["runtime_revision"], 1);
    assert_eq!(provider["config"], json!({"internal":"not-for-dashboard"}));
    assert_eq!(provider["credential"]["version"], 1);
    assert_eq!(provider["credential"]["encryption_key_version"], 1);
    assert_eq!(provider["credential"]["expires_at"], Value::Null);
    assert_eq!(provider["credential"]["refreshed_at"], Value::Null);
}

#[tokio::test]
async fn provider_detail_maps_not_found_and_rejects_invalid_ids_locally() {
    let calls = Arc::new(AtomicUsize::new(0));
    let counter = calls.clone();
    let gateway = serve(Router::new().fallback(move || async move {
        counter.fetch_add(1, Ordering::SeqCst);
        (StatusCode::NOT_FOUND, "secret-marker")
    }))
    .await;
    let app = platform(&gateway, Duration::from_secs(2)).await;

    let missing = reqwest::get(format!(
        "{}{PATH}/018f47a8-80cc-7b2f-9d44-6657f5f82aff",
        app.url
    ))
    .await
    .unwrap();
    assert_eq!(missing.status(), StatusCode::NOT_FOUND);
    let missing_body = missing.text().await.unwrap();
    assert_eq!(
        serde_json::from_str::<Value>(&missing_body).unwrap()["error"]["code"],
        "PROVIDER_NOT_FOUND"
    );
    assert!(!missing_body.contains("secret-marker"));

    let invalid = reqwest::get(format!("{}{PATH}/not-a-uuid", app.url))
        .await
        .unwrap();
    assert_eq!(invalid.status(), StatusCode::BAD_REQUEST);
    assert_eq!(
        serde_json::from_str::<Value>(&invalid.text().await.unwrap()).unwrap()["error"]["code"],
        "INVALID_PROVIDER_ID"
    );
    assert_eq!(calls.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn empty_accounts_returns_an_empty_array() {
    let gateway =
        serve(Router::new().route(GATEWAY_PATH, get(|| async { Json(json!({"accounts":[]})) })))
            .await;
    let app = platform(&gateway, Duration::from_secs(2)).await;
    let response = reqwest::get(format!("{}{PATH}", app.url)).await.unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(response.json::<Value>().await.unwrap(), json!([]));
}

#[tokio::test]
async fn upstream_failures_are_sanitized_and_redirects_are_not_followed() {
    for (upstream, expected) in [
        (401, 502),
        (403, 502),
        (429, 503),
        (500, 502),
        (504, 504),
        (302, 502),
    ] {
        let calls = Arc::new(AtomicUsize::new(0));
        let counter = calls.clone();
        let gateway = serve(Router::new().fallback(move || async move {
            counter.fetch_add(1, Ordering::SeqCst);
            (
                StatusCode::from_u16(upstream).unwrap(),
                [("location", "/redirected")],
                "secret-marker",
            )
        }))
        .await;
        let app = platform(&gateway, Duration::from_secs(2)).await;
        let response = reqwest::get(format!("{}{PATH}", app.url)).await.unwrap();
        assert_eq!(response.status().as_u16(), expected);
        assert_eq!(response.headers()["cache-control"], "no-store");
        let body = response.text().await.unwrap();
        assert!(!body.contains("secret-marker"));
        assert!(!body.contains("admin-only-token"));
        assert!(serde_json::from_str::<Value>(&body).unwrap()["error"]["code"].is_string());
        assert_eq!(calls.load(Ordering::SeqCst), 1);
    }
}

#[tokio::test]
async fn malformed_upstream_contract_fails_instead_of_returning_empty_or_partial_data() {
    for body in [
        "not json secret-marker".to_owned(),
        "{}".to_owned(),
        json!({"accounts":[{"name":"missing required fields"}]}).to_string(),
        json!({"accounts":[account(1,"openai","unknown-status","2026-09-03T00:00:00Z")]})
            .to_string(),
        json!({"accounts":[account(1,"openai","active","not-a-date")]}).to_string(),
    ] {
        let gateway = serve(Router::new().fallback(move || async move { body })).await;
        let app = platform(&gateway, Duration::from_secs(2)).await;
        let response = reqwest::get(format!("{}{PATH}", app.url)).await.unwrap();
        assert_eq!(response.status(), StatusCode::BAD_GATEWAY);
        assert!(!response.text().await.unwrap().contains("secret-marker"));
    }
}

#[tokio::test]
async fn oversized_response_is_rejected() {
    let gateway = serve(Router::new().fallback(|| async { "x".repeat(4 * 1024 * 1024 + 1) })).await;
    let app = platform(&gateway, Duration::from_secs(2)).await;
    let response = reqwest::get(format!("{}{PATH}", app.url)).await.unwrap();
    assert_eq!(response.status(), StatusCode::BAD_GATEWAY);
}

#[tokio::test]
async fn gateway_timeout_returns_504() {
    let gateway = serve(Router::new().fallback(|| async {
        tokio::time::sleep(Duration::from_secs(1)).await;
        Json(json!({"accounts":[]}))
    }))
    .await;
    let app = platform(&gateway, Duration::from_millis(100)).await;
    let response = reqwest::get(format!("{}{PATH}", app.url)).await.unwrap();
    assert_eq!(response.status(), StatusCode::GATEWAY_TIMEOUT);
    assert_eq!(
        response.json::<Value>().await.unwrap()["error"]["code"],
        "LLM_GATEWAY_TIMEOUT"
    );
}

#[test]
fn client_rejects_unsafe_configuration() {
    for url in [
        "http://example.com",
        "https://user:password@example.com",
        "https://example.com?token=secret",
        "https://example.com#fragment",
    ] {
        assert!(
            LlmGatewayClient::new(url.parse().unwrap(), "test-token", Duration::from_secs(1))
                .is_err()
        );
    }
    for token in ["", "   ", " trailing", "line\nbreak"] {
        assert!(
            LlmGatewayClient::new(
                "https://example.com".parse().unwrap(),
                token,
                Duration::from_secs(1)
            )
            .is_err()
        );
    }
    assert!(
        LlmGatewayClient::new(
            "https://example.com".parse().unwrap(),
            "test-token",
            Duration::ZERO
        )
        .is_err()
    );
}
