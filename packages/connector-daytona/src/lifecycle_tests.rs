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
async fn creates_a_default_sandbox_with_auto_stop() {
    async fn create(headers: HeaderMap, Json(body): Json<Value>) -> Json<Value> {
        assert_eq!(headers["authorization"], "Bearer daytona-secret");
        assert_eq!(
            body,
            json!({"autoStopInterval": 15, "name": "agent-pane-sandbox"})
        );
        Json(json!({"id": "sandbox-1"}))
    }
    let base = serve(Router::new().route("/api/sandbox", post(create))).await;
    let sandbox_id = create_at(
        &Client::new(),
        base,
        "daytona-secret",
        Some("agent-pane-sandbox"),
    )
    .await
    .unwrap();
    assert_eq!(sandbox_id, "sandbox-1");
}

#[tokio::test]
async fn creates_a_sandbox_from_the_snapshot_reference() {
    async fn create(headers: HeaderMap, Json(body): Json<Value>) -> Json<Value> {
        assert_eq!(headers["authorization"], "Bearer daytona-secret");
        assert_eq!(
            body,
            json!({"autoStopInterval": 15, "snapshot": "snapshot-1"})
        );
        Json(json!({"id": "sandbox-1"}))
    }
    let base = serve(Router::new().route("/api/sandbox", post(create))).await;
    let sandbox_id = create_from_snapshot_at(&Client::new(), base, "daytona-secret", "snapshot-1")
        .await
        .unwrap();
    assert_eq!(sandbox_id, "sandbox-1");
}

#[tokio::test]
async fn starts_a_stopped_sandbox_before_returning_it() {
    async fn get(State(started): State<Arc<AtomicBool>>) -> Json<Value> {
        Json(json!({
            "state": if started.load(Ordering::SeqCst) { "started" } else { "stopped" },
            "toolboxProxyUrl": "https://toolbox.example/toolbox"
        }))
    }
    async fn start(State(started): State<Arc<AtomicBool>>) -> StatusCode {
        started.store(true, Ordering::SeqCst);
        StatusCode::NO_CONTENT
    }
    let started = Arc::new(AtomicBool::new(false));
    let base = serve(
        Router::new()
            .route("/api/sandbox/sandbox-1", axum::routing::get(get))
            .route("/api/sandbox/sandbox-1/start", post(start))
            .with_state(started.clone()),
    )
    .await;
    let ready = ensure_started_at(
        &Client::new(),
        base,
        "daytona-secret",
        "sandbox-1",
        Duration::from_secs(1),
        Duration::from_millis(1),
    )
    .await
    .unwrap();
    assert!(started.load(Ordering::SeqCst));
    assert_eq!(
        ready.toolbox_url.unwrap().as_str(),
        "https://toolbox.example/toolbox/sandbox-1/"
    );
}

#[tokio::test]
async fn snapshots_a_stopped_container_and_starts_it_again() {
    async fn get(State(started): State<Arc<AtomicBool>>) -> Json<Value> {
        Json(json!({
            "state": if started.load(Ordering::SeqCst) { "started" } else { "stopped" }
        }))
    }
    async fn start(State(started): State<Arc<AtomicBool>>) -> StatusCode {
        started.store(true, Ordering::SeqCst);
        StatusCode::NO_CONTENT
    }
    async fn stop(State(started): State<Arc<AtomicBool>>) -> StatusCode {
        started.store(false, Ordering::SeqCst);
        StatusCode::NO_CONTENT
    }
    async fn snapshot(
        State(started): State<Arc<AtomicBool>>,
        Json(body): Json<Value>,
    ) -> StatusCode {
        assert!(!started.load(Ordering::SeqCst));
        assert_eq!(
            body,
            json!({"name": "agent-pane-snapshot", "includeMemory": false})
        );
        StatusCode::NO_CONTENT
    }
    let started = Arc::new(AtomicBool::new(true));
    let base = serve(
        Router::new()
            .route("/api/sandbox/sandbox-1", axum::routing::get(get))
            .route("/api/sandbox/sandbox-1/start", post(start))
            .route("/api/sandbox/sandbox-1/stop", post(stop))
            .route("/api/sandbox/sandbox-1/snapshot", post(snapshot))
            .with_state(started.clone()),
    )
    .await;
    let snapshot_name = create_snapshot_at(
        &Client::new(),
        base,
        "daytona-secret",
        "sandbox-1",
        "agent-pane-snapshot",
        Duration::from_secs(1),
        Duration::from_millis(1),
    )
    .await
    .unwrap();
    assert_eq!(snapshot_name, "agent-pane-snapshot");
    assert!(started.load(Ordering::SeqCst));
}

#[tokio::test]
async fn resolves_the_ready_toolbox_and_terminates() {
    async fn get(headers: HeaderMap) -> Json<Value> {
        assert_eq!(headers["authorization"], "Bearer daytona-secret");
        Json(json!({
            "state": "started",
            "toolboxProxyUrl": "https://toolbox.example/toolbox",
            "networkBlockAll": true
        }))
    }
    async fn delete(headers: HeaderMap) -> StatusCode {
        assert_eq!(headers["authorization"], "Bearer daytona-secret");
        StatusCode::NO_CONTENT
    }
    let base = serve(Router::new().route(
        "/api/sandbox/sandbox-1",
        axum::routing::get(get).delete(delete),
    ))
    .await;
    let ready = wait_until_ready_at(
        &Client::new(),
        base.clone(),
        "daytona-secret",
        "sandbox-1",
        Duration::from_secs(1),
        Duration::from_millis(1),
    )
    .await
    .unwrap();
    assert!(ready.network_block_all);
    terminate_at(&Client::new(), base, "daytona-secret", "sandbox-1")
        .await
        .unwrap();
}

async fn serve(app: Router) -> Url {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    Url::parse(&format!("http://{address}/api/")).unwrap()
}
