use axum::{Json, Router, http::HeaderMap, http::StatusCode, routing::post};
use serde_json::{Value, json};
use tokio::net::TcpListener;

use super::*;

#[tokio::test]
async fn resolves_the_only_available_workspace() {
    async fn list(headers: HeaderMap) -> Json<Value> {
        assert_eq!(headers["authorization"], "Bearer blaxel-secret");
        Json(json!([{"name": "workspace-1"}]))
    }
    let base = serve(Router::new().route("/v0/workspaces", axum::routing::get(list))).await;
    let workspace = resolve_workspace_at(&Client::new(), base, "blaxel-secret")
        .await
        .unwrap();
    assert_eq!(workspace, "workspace-1");
}

#[tokio::test]
async fn creates_a_default_sandbox() {
    async fn create(headers: HeaderMap, Json(body): Json<Value>) -> Json<Value> {
        assert_eq!(headers["authorization"], "Bearer blaxel-secret");
        assert_eq!(headers["x-blaxel-workspace"], "workspace-1");
        assert_eq!(
            body,
            json!({
                "metadata": {"name": "sandbox-1"},
                "spec": {"region": "auto"},
            })
        );
        Json(json!({"status": "DEPLOYING"}))
    }
    let base = serve(Router::new().route("/v0/sandboxes", post(create))).await;
    let sandbox_id = create_at(
        &Client::new(),
        base,
        "blaxel-secret",
        "workspace-1",
        "sandbox-1",
    )
    .await
    .unwrap();
    assert_eq!(sandbox_id, "sandbox-1");
}

#[tokio::test]
async fn creates_a_ready_snapshot() {
    async fn snapshot(headers: HeaderMap, Json(body): Json<Value>) -> Json<Value> {
        assert_eq!(headers["authorization"], "Bearer blaxel-secret");
        assert_eq!(headers["x-blaxel-workspace"], "workspace-1");
        assert_eq!(body, json!({"name": "snapshot-1"}));
        Json(json!({"id": "provider-snapshot-1", "status": "ready"}))
    }
    let base =
        serve(Router::new().route("/v0/sandboxes/sandbox-1/snapshots", post(snapshot))).await;
    let snapshot_id = create_snapshot_at(
        &Client::new(),
        base,
        "blaxel-secret",
        "workspace-1",
        "sandbox-1",
        "snapshot-1",
        Duration::from_secs(1),
        Duration::from_millis(1),
    )
    .await
    .unwrap();
    assert_eq!(snapshot_id, "provider-snapshot-1");
}

#[tokio::test]
async fn forks_the_source_sandbox_at_the_requested_snapshot() {
    async fn create(headers: HeaderMap, Json(body): Json<Value>) -> Json<Value> {
        assert_eq!(headers["authorization"], "Bearer blaxel-secret");
        assert_eq!(headers["x-blaxel-workspace"], "workspace-1");
        assert_eq!(
            body,
            json!({
                "targetType": "sandbox",
                "targetName": "target-1",
                "snapshotId": "snapshot-1",
            })
        );
        Json(json!({"status": "DEPLOYING"}))
    }
    let base = serve(Router::new().route("/v0/sandboxes/source-1/fork", post(create))).await;
    let sandbox_id = create_from_snapshot_at(
        &Client::new(),
        base,
        "blaxel-secret",
        "workspace-1",
        "source-1",
        "snapshot-1",
        "target-1",
    )
    .await
    .unwrap();
    assert_eq!(sandbox_id, "target-1");
}

#[tokio::test]
async fn resolves_the_ready_sandbox_url_and_terminates() {
    async fn get(headers: HeaderMap) -> Json<Value> {
        assert_eq!(headers["authorization"], "Bearer blaxel-secret");
        assert_eq!(headers["x-blaxel-workspace"], "workspace-1");
        Json(json!({
            "status": "DEPLOYED",
            "metadata": {"url": "https://sandbox.example/"}
        }))
    }
    async fn delete(headers: HeaderMap) -> StatusCode {
        assert_eq!(headers["authorization"], "Bearer blaxel-secret");
        StatusCode::OK
    }
    let base = serve(Router::new().route(
        "/v0/sandboxes/sandbox-1",
        axum::routing::get(get).delete(delete),
    ))
    .await;
    let ready = wait_until_ready_at(
        &Client::new(),
        base.clone(),
        "blaxel-secret",
        "workspace-1",
        "sandbox-1",
        Duration::from_secs(1),
        Duration::from_millis(1),
    )
    .await
    .unwrap();
    assert_eq!(ready.sandbox_url.as_str(), "https://sandbox.example/");
    terminate_at(
        &Client::new(),
        base,
        "blaxel-secret",
        "workspace-1",
        "sandbox-1",
    )
    .await
    .unwrap();
}

async fn serve(app: Router) -> Url {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    Url::parse(&format!("http://{address}/v0/")).unwrap()
}
