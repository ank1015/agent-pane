use std::time::Duration;

use axum::{
    Json, Router,
    extract::State,
    http::{HeaderMap, StatusCode},
    routing::post,
};
use serde_json::{Value, json};
use tokio::{net::TcpListener, sync::mpsc};

use super::ExecutionGatewayClient;
use crate::machines::model::{
    CreateMachineEnvironmentRequest, CreateSandboxAccountRequest, MachineInventory, SandboxProvider,
};

#[tokio::test]
async fn create_sandbox_account_authenticates_and_forwards_the_secret_once() {
    let (request_tx, mut request_rx) = mpsc::unbounded_channel();
    let mock_gateway = Router::new()
        .route("/v1/control/sandbox-accounts", post(capture_create_request))
        .with_state(request_tx);
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    tokio::spawn(async move {
        axum::serve(listener, mock_gateway).await.unwrap();
    });

    let client = ExecutionGatewayClient::new(
        format!("http://{address}").parse().unwrap(),
        "platform-control-token",
        Duration::from_secs(1),
    )
    .unwrap();
    let response = client
        .create_sandbox_account(&CreateSandboxAccountRequest {
            provider: SandboxProvider::E2b,
            name: "main".to_owned(),
            api_key: "secret-key".to_owned(),
            config: json!({}),
            enabled: true,
            make_default: false,
        })
        .await
        .unwrap();
    let (authorization, body) = request_rx.recv().await.unwrap();

    assert_eq!(authorization, "Bearer platform-control-token");
    assert_eq!(
        body,
        json!({"provider": "e2b", "name": "main", "api_key": "secret-key"})
    );
    assert_eq!(response.name, "main");
}

#[tokio::test]
async fn machine_inventory_uses_the_control_endpoint() {
    let (request_tx, mut request_rx) = mpsc::unbounded_channel();
    let mock_gateway = Router::new()
        .route(
            "/v1/control/machines",
            axum::routing::get(capture_inventory_request),
        )
        .with_state(request_tx);
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    tokio::spawn(async move {
        axum::serve(listener, mock_gateway).await.unwrap();
    });

    let client = ExecutionGatewayClient::new(
        format!("http://{address}").parse().unwrap(),
        "platform-control-token",
        Duration::from_secs(1),
    )
    .unwrap();
    let inventory: MachineInventory = client.machine_inventory().await.unwrap();

    assert_eq!(
        request_rx.recv().await.unwrap(),
        "Bearer platform-control-token"
    );
    assert!(inventory.connector_accounts.is_empty());
    assert!(inventory.machine_daemons.is_empty());
}

#[tokio::test]
async fn environment_list_filters_by_machine_and_authenticates() {
    let (request_tx, mut request_rx) = mpsc::unbounded_channel();
    let mock_gateway = Router::new()
        .route(
            "/v1/control/environments",
            axum::routing::get(capture_environment_list_request),
        )
        .with_state(request_tx);
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    tokio::spawn(async move {
        axum::serve(listener, mock_gateway).await.unwrap();
    });

    let client = ExecutionGatewayClient::new(
        format!("http://{address}").parse().unwrap(),
        "platform-control-token",
        Duration::from_secs(1),
    )
    .unwrap();
    let environments = client.list_environments("machine-1").await.unwrap();
    let (authorization, query) = request_rx.recv().await.unwrap();

    assert_eq!(authorization, "Bearer platform-control-token");
    assert_eq!(query.as_deref(), Some("machine_id=machine-1"));
    assert_eq!(environments.len(), 1);
    assert_eq!(environments[0].machine_id.as_str(), "machine-1");
    assert_eq!(environments[0].path, "projects/example");
}

#[tokio::test]
async fn environment_create_forwards_the_machine_and_location() {
    let (request_tx, mut request_rx) = mpsc::unbounded_channel();
    let mock_gateway = Router::new()
        .route(
            "/v1/control/environments",
            post(capture_environment_create_request),
        )
        .with_state(request_tx);
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    tokio::spawn(async move {
        axum::serve(listener, mock_gateway).await.unwrap();
    });

    let client = ExecutionGatewayClient::new(
        format!("http://{address}").parse().unwrap(),
        "platform-control-token",
        Duration::from_secs(1),
    )
    .unwrap();
    let environment = client
        .create_environment(
            "machine-1",
            &CreateMachineEnvironmentRequest {
                name: "Development".to_owned(),
                workspace_root_id: "root".to_owned(),
                path: "projects/example".to_owned(),
            },
        )
        .await
        .unwrap();
    let (authorization, body) = request_rx.recv().await.unwrap();

    assert_eq!(authorization, "Bearer platform-control-token");
    assert_eq!(
        body,
        json!({
            "machine_id": "machine-1",
            "name": "Development",
            "workspace_root_id": "root",
            "path": "projects/example"
        })
    );
    assert_eq!(environment.name, "Development");
}

#[tokio::test]
async fn environment_name_update_forwards_the_name() {
    let (request_tx, mut request_rx) = mpsc::unbounded_channel();
    let mock_gateway = Router::new()
        .route(
            "/v1/control/environments/environment-1",
            axum::routing::patch(capture_environment_name_update_request),
        )
        .with_state(request_tx);
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    tokio::spawn(async move {
        axum::serve(listener, mock_gateway).await.unwrap();
    });

    let client = ExecutionGatewayClient::new(
        format!("http://{address}").parse().unwrap(),
        "platform-control-token",
        Duration::from_secs(1),
    )
    .unwrap();
    let environment = client
        .update_environment_name(
            "environment-1",
            &crate::machines::model::UpdateNameRequest {
                name: "Renamed".to_owned(),
            },
        )
        .await
        .unwrap();
    let (authorization, body) = request_rx.recv().await.unwrap();

    assert_eq!(authorization, "Bearer platform-control-token");
    assert_eq!(body, json!({"name": "Renamed"}));
    assert_eq!(environment.name, "Renamed");
}

async fn capture_create_request(
    State(request_tx): State<mpsc::UnboundedSender<(String, Value)>>,
    headers: HeaderMap,
    Json(body): Json<Value>,
) -> (StatusCode, Json<Value>) {
    let authorization = headers
        .get("authorization")
        .and_then(|value| value.to_str().ok())
        .unwrap_or_default()
        .to_owned();
    request_tx.send((authorization, body)).unwrap();
    (
        StatusCode::CREATED,
        Json(json!({
            "id": "01992aa0-0000-7000-8000-000000000001",
            "provider": "e2b",
            "name": "main",
            "config": {},
            "enabled": true,
            "is_default": true,
            "validation_status": "unchecked",
            "credential_version": 1,
            "credentials_updated_at": 1787443200000_u64,
            "created_at": 1787443200000_u64,
            "updated_at": 1787443200000_u64
        })),
    )
}

async fn capture_inventory_request(
    State(request_tx): State<mpsc::UnboundedSender<String>>,
    headers: HeaderMap,
) -> Json<Value> {
    let authorization = headers
        .get("authorization")
        .and_then(|value| value.to_str().ok())
        .unwrap_or_default()
        .to_owned();
    request_tx.send(authorization).unwrap();
    Json(json!({"connector_accounts": [], "machine_daemons": []}))
}

async fn capture_environment_list_request(
    State(request_tx): State<mpsc::UnboundedSender<(String, Option<String>)>>,
    headers: HeaderMap,
    axum::extract::RawQuery(query): axum::extract::RawQuery,
) -> Json<Value> {
    let authorization = headers
        .get("authorization")
        .and_then(|value| value.to_str().ok())
        .unwrap_or_default()
        .to_owned();
    request_tx.send((authorization, query)).unwrap();
    Json(json!([{
        "environment_id": "01992aa0-0000-7000-8000-000000000002",
        "machine_id": "machine-1",
        "name": "Example",
        "workspace_root_id": "root",
        "path": "projects/example",
        "created_at": 1787443200000_u64
    }]))
}

async fn capture_environment_create_request(
    State(request_tx): State<mpsc::UnboundedSender<(String, Value)>>,
    headers: HeaderMap,
    Json(body): Json<Value>,
) -> (StatusCode, Json<Value>) {
    let authorization = headers
        .get("authorization")
        .and_then(|value| value.to_str().ok())
        .unwrap_or_default()
        .to_owned();
    request_tx.send((authorization, body)).unwrap();
    (
        StatusCode::CREATED,
        Json(json!({
            "environment_id": "01992aa0-0000-7000-8000-000000000002",
            "machine_id": "machine-1",
            "name": "Development",
            "workspace_root_id": "root",
            "path": "projects/example",
            "created_at": 1787443200000_u64
        })),
    )
}

async fn capture_environment_name_update_request(
    State(request_tx): State<mpsc::UnboundedSender<(String, Value)>>,
    headers: HeaderMap,
    Json(body): Json<Value>,
) -> Json<Value> {
    let authorization = headers
        .get("authorization")
        .and_then(|value| value.to_str().ok())
        .unwrap_or_default()
        .to_owned();
    request_tx.send((authorization, body)).unwrap();
    Json(json!({
        "environment_id": "environment-1",
        "machine_id": "machine-1",
        "name": "Renamed",
        "workspace_root_id": "root",
        "path": "projects/example",
        "created_at": 1787443200000_u64
    }))
}
