use std::sync::{
    Arc,
    atomic::{AtomicBool, Ordering},
};

use axum::{
    Json, Router,
    extract::State,
    http::{HeaderMap, StatusCode},
    routing::post,
};
use serde_json::{Value, json};
use tokio::net::TcpListener;

use super::*;

#[tokio::test]
async fn creates_a_named_sandbox_with_idle_suspend() {
    async fn create(headers: HeaderMap, Json(body): Json<Value>) -> Json<Value> {
        assert_eq!(headers["authorization"], "Bearer tensorlake-secret");
        assert_eq!(
            body,
            json!({
                "name": "agent-pane-sandbox",
                "timeout_secs": 600,
            })
        );
        Json(json!({"sandbox_id": "sandbox-1", "status": "pending"}))
    }
    let base = serve(Router::new().route("/sandboxes", post(create))).await;
    let sandbox_id = create_at(
        &Client::new(),
        base,
        "tensorlake-secret",
        "agent-pane-sandbox",
    )
    .await
    .unwrap();
    assert_eq!(sandbox_id, "sandbox-1");
}

#[tokio::test]
async fn creates_a_named_sandbox_from_a_snapshot() {
    async fn create(headers: HeaderMap, Json(body): Json<Value>) -> Json<Value> {
        assert_eq!(headers["authorization"], "Bearer tensorlake-secret");
        assert_eq!(
            body,
            json!({
                "snapshot_id": "snapshot-1",
                "name": "agent-pane-copy",
                "timeout_secs": 600,
            })
        );
        Json(json!({"sandbox_id": "sandbox-1", "status": "pending"}))
    }
    let base = serve(Router::new().route("/sandboxes", post(create))).await;
    let sandbox_id = create_from_snapshot_at(
        &Client::new(),
        base,
        "tensorlake-secret",
        "snapshot-1",
        "agent-pane-copy",
    )
    .await
    .unwrap();
    assert_eq!(sandbox_id, "sandbox-1");
}

#[tokio::test]
async fn resumes_a_suspended_sandbox_before_returning_it() {
    async fn get(State(running): State<Arc<AtomicBool>>) -> Json<Value> {
        Json(json!({
            "status": if running.load(Ordering::SeqCst) { "running" } else { "suspended" },
            "sandbox_url": "https://sandbox.tensorlake.example/",
        }))
    }
    async fn resume(State(running): State<Arc<AtomicBool>>) -> StatusCode {
        running.store(true, Ordering::SeqCst);
        StatusCode::ACCEPTED
    }
    let running = Arc::new(AtomicBool::new(false));
    let base = serve(
        Router::new()
            .route("/sandboxes/sandbox-1", axum::routing::get(get))
            .route("/sandboxes/sandbox-1/resume", post(resume))
            .with_state(running.clone()),
    )
    .await;
    let ready = ensure_started_at(
        &Client::new(),
        base,
        "tensorlake-secret",
        "sandbox-1",
        Duration::from_secs(1),
        Duration::from_millis(1),
    )
    .await
    .unwrap();
    assert!(running.load(Ordering::SeqCst));
    assert_eq!(
        ready.sandbox_url.as_str(),
        "https://sandbox.tensorlake.example/"
    );
}

#[tokio::test]
async fn creates_a_filesystem_snapshot_and_waits_until_restorable() {
    async fn get_sandbox() -> Json<Value> {
        Json(json!({
            "status": "running",
            "sandbox_url": "https://sandbox.tensorlake.example/",
        }))
    }
    async fn create_snapshot(Json(body): Json<Value>) -> Json<Value> {
        assert_eq!(body, json!({"snapshot_type": "filesystem"}));
        Json(json!({
            "snapshot_id": "snapshot-1",
            "status": "in_progress",
        }))
    }
    async fn get_snapshot() -> Json<Value> {
        Json(json!({
            "snapshot_id": "snapshot-1",
            "status": "local_ready",
        }))
    }
    let base = serve(
        Router::new()
            .route("/sandboxes/sandbox-1", axum::routing::get(get_sandbox))
            .route("/sandboxes/sandbox-1/snapshot", post(create_snapshot))
            .route("/snapshots/snapshot-1", axum::routing::get(get_snapshot)),
    )
    .await;
    let snapshot_id = create_snapshot_at(
        &Client::new(),
        base,
        "tensorlake-secret",
        "sandbox-1",
        Duration::from_secs(1),
        Duration::from_millis(1),
    )
    .await
    .unwrap();
    assert_eq!(snapshot_id, "snapshot-1");
}

#[tokio::test]
async fn reports_provider_snapshot_failures() {
    async fn get_sandbox() -> Json<Value> {
        Json(json!({
            "status": "running",
            "sandbox_url": "https://sandbox.tensorlake.example/",
        }))
    }
    async fn create_snapshot() -> Json<Value> {
        Json(json!({
            "snapshot_id": "snapshot-1",
            "status": "in_progress",
        }))
    }
    async fn get_snapshot() -> Json<Value> {
        Json(json!({
            "snapshot_id": "snapshot-1",
            "status": "failed",
            "error": "capture failed",
        }))
    }
    let base = serve(
        Router::new()
            .route("/sandboxes/sandbox-1", axum::routing::get(get_sandbox))
            .route("/sandboxes/sandbox-1/snapshot", post(create_snapshot))
            .route("/snapshots/snapshot-1", axum::routing::get(get_snapshot)),
    )
    .await;
    let error = create_snapshot_at(
        &Client::new(),
        base,
        "tensorlake-secret",
        "sandbox-1",
        Duration::from_secs(1),
        Duration::from_millis(1),
    )
    .await
    .unwrap_err();
    assert!(error.message.contains("capture failed"));
}

#[tokio::test]
async fn resolves_the_running_sandbox_url_and_terminates() {
    async fn get(headers: HeaderMap) -> Json<Value> {
        assert_eq!(headers["authorization"], "Bearer tensorlake-secret");
        Json(json!({
            "status": "running",
            "sandbox_url": "https://sandbox.tensorlake.example/"
        }))
    }
    async fn delete(headers: HeaderMap) -> StatusCode {
        assert_eq!(headers["authorization"], "Bearer tensorlake-secret");
        StatusCode::NO_CONTENT
    }
    let base = serve(Router::new().route(
        "/sandboxes/sandbox-1",
        axum::routing::get(get).delete(delete),
    ))
    .await;
    let ready = wait_until_ready_at(
        &Client::new(),
        base.clone(),
        "tensorlake-secret",
        "sandbox-1",
        Duration::from_secs(1),
        Duration::from_millis(1),
    )
    .await
    .unwrap();
    assert_eq!(
        ready.sandbox_url.as_str(),
        "https://sandbox.tensorlake.example/"
    );
    terminate_at(&Client::new(), base, "tensorlake-secret", "sandbox-1")
        .await
        .unwrap();
}

async fn serve(app: Router) -> Url {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    Url::parse(&format!("http://{address}/")).unwrap()
}
