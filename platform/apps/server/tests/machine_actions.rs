use axum::{
    Json, Router,
    extract::Path,
    http::{HeaderMap, StatusCode},
    routing::get,
};
use platform_server::machines::{self, ExecutionGatewayClient, MachineService};
use serde_json::{Value, json};
use std::{
    sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    },
    time::Duration,
};
use tokio::{net::TcpListener, task::JoinHandle};

const ID: &str = "018f47a8-80cc-7b2f-9d44-6657f5f82ad0";
fn host() -> Value {
    json!({"id":ID,"kind":"registered","name":"Development Mac","desired_state":"ready","state":"ready",
        "status_code":null,"status_message":null,"status_retryable":false,"descriptor":null,"metadata":{},"e2b":null,
        "registered":{"installation_id":null,"daemon_version":null,"protocol_version":null,"registered_at":null,"last_connected_at":null,"last_disconnected_at":null},
        "last_seen_at":null,"revision":1,"created_at":"2026-09-03T00:00:00Z","updated_at":"2026-09-03T00:00:00Z","deleted_at":null})
}
struct Server {
    url: String,
    task: JoinHandle<()>,
}
impl Drop for Server {
    fn drop(&mut self) {
        self.task.abort();
    }
}
async fn serve(router: Router) -> Server {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}", listener.local_addr().unwrap());
    Server {
        url,
        task: tokio::spawn(async move { axum::serve(listener, router).await.unwrap() }),
    }
}
async fn platform(gateway: &Server, timeout: Duration) -> Server {
    serve(machines::router(MachineService::new(
        ExecutionGatewayClient::new(gateway.url.parse().unwrap(), "test-token", timeout).unwrap(),
    )))
    .await
}

#[tokio::test]
async fn rename_and_delete_forward_exact_payload_and_auth() {
    let writes = Arc::new(AtomicUsize::new(0));
    let updates = writes.clone();
    let deletes = writes.clone();
    let gateway = serve(
        Router::new().route(
            "/v1/hosts/{id}",
            get(|Path(id): Path<String>, headers: HeaderMap| async move {
                assert_eq!(id, ID);
                assert_eq!(headers["authorization"], "Bearer test-token");
                Json(host())
            })
            .patch(move |headers: HeaderMap, Json(payload): Json<Value>| {
                let updates = updates.clone();
                async move {
                    updates.fetch_add(1, Ordering::SeqCst);
                    assert_eq!(headers["authorization"], "Bearer test-token");
                    assert_eq!(payload, json!({"name":"Renamed Mac"}));
                    let mut result = host();
                    result["name"] = json!("Renamed Mac");
                    Json(result)
                }
            })
            .delete(move |headers: HeaderMap| {
                let deletes = deletes.clone();
                async move {
                    deletes.fetch_add(1, Ordering::SeqCst);
                    assert_eq!(headers["authorization"], "Bearer test-token");
                    let mut result = host();
                    result["state"] = json!("deleted");
                    result["deleted_at"] = json!("2026-09-04T00:00:00Z");
                    (StatusCode::ACCEPTED, Json(result))
                }
            }),
        ),
    )
    .await;
    let app = platform(&gateway, Duration::from_secs(1)).await;
    let url = format!("{}/api/machines/{ID}", app.url);
    let client = reqwest::Client::new();
    let renamed = client
        .patch(&url)
        .json(&json!({"name":"Renamed Mac"}))
        .send()
        .await
        .unwrap();
    assert_eq!(renamed.status(), StatusCode::OK);
    assert_eq!(renamed.headers()["cache-control"], "no-store");
    assert_eq!(
        renamed.json::<Value>().await.unwrap()["name"],
        "Renamed Mac"
    );
    let deleted = client.delete(&url).send().await.unwrap();
    assert_eq!(deleted.status(), StatusCode::NO_CONTENT);
    assert_eq!(deleted.headers()["cache-control"], "no-store");
    assert!(deleted.text().await.unwrap().is_empty());
    assert_eq!(writes.load(Ordering::SeqCst), 2);
}

#[tokio::test]
async fn invalid_inputs_do_not_contact_gateway() {
    let calls = Arc::new(AtomicUsize::new(0));
    let counter = calls.clone();
    let gateway = serve(Router::new().fallback(move || {
        counter.fetch_add(1, Ordering::SeqCst);
        async { StatusCode::INTERNAL_SERVER_ERROR }
    }))
    .await;
    let app = platform(&gateway, Duration::from_secs(1)).await;
    let client = reqwest::Client::new();
    for payload in [
        json!({}),
        json!({"name":null}),
        json!({"name":""}),
        json!({"name":" bad"}),
        json!({"name":"bad\nname"}),
        json!({"name":"a".repeat(201)}),
        json!({"name":"ok","metadata":{"secret-marker":true}}),
        json!({"name":"a".repeat(17000)}),
    ] {
        let response = client
            .patch(format!("{}/api/machines/{ID}", app.url))
            .json(&payload)
            .send()
            .await
            .unwrap();
        assert!(response.status().is_client_error());
        assert_eq!(response.headers()["cache-control"], "no-store");
        assert!(!response.text().await.unwrap().contains("secret-marker"));
    }
    for method in [reqwest::Method::PATCH, reqwest::Method::DELETE] {
        let response = client
            .request(method, format!("{}/api/machines/not-a-uuid", app.url))
            .json(&json!({"name":"Good"}))
            .send()
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    }
    assert_eq!(calls.load(Ordering::SeqCst), 0);
}

#[tokio::test]
async fn sandbox_hosts_are_rejected_and_deleted_registrations_are_idempotent() {
    for (kind, state, expected) in [("e2b", "ready", 400), ("registered", "deleted", 204)] {
        let writes = Arc::new(AtomicUsize::new(0));
        let counter = writes.clone();
        let gateway = serve(
            Router::new().route(
                "/v1/hosts/{id}",
                get(move || async move {
                    let mut result = host();
                    result["kind"] = json!(kind);
                    result["state"] = json!(state);
                    Json(result)
                })
                .delete(move || {
                    counter.fetch_add(1, Ordering::SeqCst);
                    async { StatusCode::INTERNAL_SERVER_ERROR }
                }),
            ),
        )
        .await;
        let app = platform(&gateway, Duration::from_secs(1)).await;
        let response = reqwest::Client::new()
            .delete(format!("{}/api/machines/{ID}", app.url))
            .send()
            .await
            .unwrap();
        assert_eq!(response.status().as_u16(), expected);
        assert_eq!(writes.load(Ordering::SeqCst), 0);
    }
}

#[tokio::test]
async fn mutation_errors_are_sanitized_and_not_retried() {
    for (status, expected) in [
        (400, 400),
        (404, 404),
        (409, 409),
        (429, 429),
        (401, 502),
        (403, 502),
        (500, 502),
        (307, 502),
        (504, 504),
    ] {
        let calls = Arc::new(AtomicUsize::new(0));
        let counter = calls.clone();
        let gateway = serve(Router::new().route(
            "/v1/hosts/{id}",
            get(|| async { Json(host()) }).patch(move || {
                counter.fetch_add(1, Ordering::SeqCst);
                async move {
                    (
                        StatusCode::from_u16(status).unwrap(),
                        [("location", "/redirected")],
                        "secret-marker",
                    )
                }
            }),
        ))
        .await;
        let app = platform(&gateway, Duration::from_secs(1)).await;
        let response = reqwest::Client::new()
            .patch(format!("{}/api/machines/{ID}", app.url))
            .json(&json!({"name":"Renamed"}))
            .send()
            .await
            .unwrap();
        assert_eq!(response.status().as_u16(), expected);
        assert_eq!(response.headers()["cache-control"], "no-store");
        assert!(!response.text().await.unwrap().contains("secret-marker"));
        assert_eq!(calls.load(Ordering::SeqCst), 1);
    }
}

#[tokio::test]
async fn invalid_success_and_timeout_do_not_report_success() {
    for body in [
        "malformed secret-marker".to_owned(),
        "x".repeat(1024 * 1024 + 1),
        host().to_string(),
    ] {
        let gateway = serve(Router::new().route(
            "/v1/hosts/{id}",
            get(|| async { Json(host()) }).delete(move || {
                let body = body.clone();
                async move { body }
            }),
        ))
        .await;
        let app = platform(&gateway, Duration::from_secs(1)).await;
        let response = reqwest::Client::new()
            .delete(format!("{}/api/machines/{ID}", app.url))
            .send()
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::BAD_GATEWAY);
        assert!(!response.text().await.unwrap().contains("secret-marker"));
    }
    let gateway = serve(Router::new().route(
        "/v1/hosts/{id}",
        get(|| async { Json(host()) }).delete(|| async {
            tokio::time::sleep(Duration::from_secs(1)).await;
            Json(host())
        }),
    ))
    .await;
    let app = platform(&gateway, Duration::from_millis(25)).await;
    let response = reqwest::Client::new()
        .delete(format!("{}/api/machines/{ID}", app.url))
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::GATEWAY_TIMEOUT);
    assert!(
        response
            .text()
            .await
            .unwrap()
            .contains("may already have completed")
    );
}
