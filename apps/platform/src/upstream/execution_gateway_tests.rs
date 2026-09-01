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
    CreateMachineEnvironmentRequest, CreateSandboxAccountRequest, CreateSandboxRequest,
    CreateSandboxSnapshotRequest, MachineInventory, SandboxProvider, SnapshotQuery,
};

const E2B_ACCOUNT_ID: &str = "01992aa0-0000-7000-8000-000000000010";
const PROJECT_ID: &str = "01992aa0-0000-7000-8000-000000000011";
const TEMPLATE_ID: &str = "01992aa0-0000-7000-8000-000000000012";

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
                project_id: PROJECT_ID.parse().unwrap(),
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
            "project_id": PROJECT_ID,
            "machine_id": "machine-1",
            "name": "Development",
            "workspace_root_id": "root",
            "path": "projects/example"
        })
    );
    assert_eq!(environment.name, "Development");
}

#[tokio::test]
async fn project_environment_list_uses_the_project_endpoint_and_authenticates() {
    let (request_tx, mut request_rx) = mpsc::unbounded_channel();
    let mock_gateway = Router::new()
        .route(
            &format!("/v1/control/projects/{PROJECT_ID}/environments"),
            axum::routing::get(capture_project_environment_list_request),
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
    let environments = client
        .list_project_environments(PROJECT_ID.parse().unwrap())
        .await
        .unwrap();

    assert_eq!(
        request_rx.recv().await.unwrap(),
        "Bearer platform-control-token"
    );
    assert_eq!(environments.len(), 2);
    assert_eq!(environments[0].host_name, "Workstation");
    assert_eq!(environments[1].host_name, "E2B main");
}

#[tokio::test]
async fn sandbox_materialization_uses_its_dedicated_longer_timeout() {
    let (request_tx, mut request_rx) = mpsc::unbounded_channel();
    let mock_gateway = Router::new()
        .route(
            &format!("/v1/control/sandbox-environment-templates/{TEMPLATE_ID}/environments"),
            post(capture_delayed_materialization_request),
        )
        .with_state(request_tx);
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    tokio::spawn(async move {
        axum::serve(listener, mock_gateway).await.unwrap();
    });

    let client = ExecutionGatewayClient::new_with_materialization_timeout(
        format!("http://{address}").parse().unwrap(),
        "platform-control-token",
        Duration::from_millis(25),
        Duration::from_millis(250),
    )
    .unwrap();
    let instance = client
        .materialize_sandbox_environment_template(TEMPLATE_ID.parse().unwrap())
        .await
        .unwrap();

    assert_eq!(
        request_rx.recv().await.unwrap(),
        "Bearer platform-control-token"
    );
    assert_eq!(instance.template_id.to_string(), TEMPLATE_ID);
    assert_eq!(instance.environment.machine_id.as_str(), "machine-e2b-1");
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

#[tokio::test]
async fn snapshot_name_update_forwards_the_name() {
    let snapshot_id = "01992aa0-0000-7000-8000-000000000020";
    let (request_tx, mut request_rx) = mpsc::unbounded_channel();
    let mock_gateway = Router::new()
        .route(
            &format!("/v1/control/snapshots/{snapshot_id}"),
            axum::routing::patch(capture_snapshot_name_update_request),
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
    let snapshot = client
        .update_snapshot_name(
            snapshot_id.parse().unwrap(),
            &crate::machines::model::UpdateNameRequest {
                name: "Renamed snapshot".to_owned(),
            },
        )
        .await
        .unwrap();
    let (authorization, body) = request_rx.recv().await.unwrap();

    assert_eq!(authorization, "Bearer platform-control-token");
    assert_eq!(body, json!({"name": "Renamed snapshot"}));
    assert_eq!(snapshot.name, "Renamed snapshot");
}

#[tokio::test]
async fn sandbox_list_uses_the_account_endpoint_and_authenticates() {
    let (request_tx, mut request_rx) = mpsc::unbounded_channel();
    let mock_gateway = Router::new()
        .route(
            &format!("/v1/control/sandbox-accounts/{E2B_ACCOUNT_ID}/sandboxes"),
            axum::routing::get(capture_sandbox_list_request),
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
    let sandboxes = client
        .list_sandbox_machines(E2B_ACCOUNT_ID.parse().unwrap())
        .await
        .unwrap();

    assert_eq!(
        request_rx.recv().await.unwrap(),
        "Bearer platform-control-token"
    );
    assert_eq!(sandboxes.len(), 1);
    assert_eq!(sandboxes[0].sandbox_id, "e2b-sandbox-1");
    assert_eq!(sandboxes[0].created_from.as_deref(), Some("base"));
}

#[tokio::test]
async fn sandbox_creation_forwards_the_selected_account_and_template() {
    let (request_tx, mut request_rx) = mpsc::unbounded_channel();
    let mock_gateway = Router::new()
        .route(
            &format!("/v1/control/sandbox-accounts/{E2B_ACCOUNT_ID}/sandboxes"),
            post(capture_sandbox_create_request),
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
    let created = client
        .create_sandbox(
            E2B_ACCOUNT_ID.parse().unwrap(),
            &CreateSandboxRequest {
                template_id: Some("base".to_owned()),
                name: Some("Development".to_owned()),
            },
        )
        .await
        .unwrap();
    let (authorization, body) = request_rx.recv().await.unwrap();

    assert_eq!(authorization, "Bearer platform-control-token");
    assert_eq!(body, json!({"template_id": "base", "name": "Development"}));
    assert_eq!(created.sandbox_id, "e2b-sandbox-1");
    assert_eq!(created.machine.name, "Development");
}

#[tokio::test]
async fn snapshot_list_filters_by_account_and_authenticates() {
    let (request_tx, mut request_rx) = mpsc::unbounded_channel();
    let mock_gateway = Router::new()
        .route(
            "/v1/control/snapshots",
            axum::routing::get(capture_snapshot_list_request),
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
    let snapshots = client
        .list_snapshots(&SnapshotQuery {
            sandbox_account_id: Some(E2B_ACCOUNT_ID.parse().unwrap()),
            ..SnapshotQuery::default()
        })
        .await
        .unwrap();
    let (authorization, query) = request_rx.recv().await.unwrap();

    assert_eq!(authorization, "Bearer platform-control-token");
    assert_eq!(
        query.as_deref(),
        Some("sandbox_account_id=01992aa0-0000-7000-8000-000000000010")
    );
    assert_eq!(snapshots.len(), 1);
    assert_eq!(snapshots[0].name, "Ready workspace");
}

#[tokio::test]
async fn sandbox_snapshot_creation_forwards_the_account_sandbox_and_name() {
    let (request_tx, mut request_rx) = mpsc::unbounded_channel();
    let mock_gateway = Router::new()
        .route(
            &format!(
                "/v1/control/sandbox-accounts/{E2B_ACCOUNT_ID}/sandboxes/e2b-sandbox-1/snapshots"
            ),
            post(capture_sandbox_snapshot_create_request),
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
    let created = client
        .create_sandbox_snapshot(
            E2B_ACCOUNT_ID.parse().unwrap(),
            "e2b-sandbox-1",
            &CreateSandboxSnapshotRequest {
                name: "Ready workspace".to_owned(),
            },
        )
        .await
        .unwrap();
    let (authorization, body) = request_rx.recv().await.unwrap();

    assert_eq!(authorization, "Bearer platform-control-token");
    assert_eq!(body, json!({"name": "Ready workspace"}));
    assert_eq!(created.name, "Ready workspace");
    assert_eq!(created.sandbox_id, "e2b-sandbox-1");
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
        "project_id": PROJECT_ID,
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
            "project_id": PROJECT_ID,
            "machine_id": "machine-1",
            "name": "Development",
            "workspace_root_id": "root",
            "path": "projects/example",
            "created_at": 1787443200000_u64
        })),
    )
}

async fn capture_project_environment_list_request(
    State(request_tx): State<mpsc::UnboundedSender<String>>,
    headers: HeaderMap,
) -> Json<Value> {
    let authorization = headers
        .get("authorization")
        .and_then(|value| value.to_str().ok())
        .unwrap_or_default()
        .to_owned();
    request_tx.send(authorization).unwrap();
    Json(json!([
        {
            "id": "environment-1",
            "name": "Local project",
            "host_name": "Workstation",
            "path": "projects/example",
            "type": "env",
            "created_at": 1787443200000_u64
        },
        {
            "id": "01992aa0-0000-7000-8000-000000000012",
            "name": "Sandbox project",
            "host_name": "E2B main",
            "path": "/workspace/project",
            "type": "template",
            "snapshot_id": "01992aa0-0000-7000-8000-000000000013",
            "created_at": 1787443200001_u64
        }
    ]))
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
        "project_id": PROJECT_ID,
        "machine_id": "machine-1",
        "name": "Renamed",
        "workspace_root_id": "root",
        "path": "projects/example",
        "created_at": 1787443200000_u64
    }))
}

async fn capture_delayed_materialization_request(
    State(request_tx): State<mpsc::UnboundedSender<String>>,
    headers: HeaderMap,
) -> (StatusCode, Json<Value>) {
    let authorization = headers
        .get("authorization")
        .and_then(|value| value.to_str().ok())
        .unwrap_or_default()
        .to_owned();
    request_tx.send(authorization).unwrap();
    tokio::time::sleep(Duration::from_millis(75)).await;
    (
        StatusCode::CREATED,
        Json(json!({
            "template_id": TEMPLATE_ID,
            "provider": "e2b",
            "provider_sandbox_id": "e2b-sandbox-1",
            "environment": {
                "environment_id": "environment-1",
                "project_id": PROJECT_ID,
                "machine_id": "machine-e2b-1",
                "name": "Development",
                "workspace_root_id": "root",
                "path": "projects/example",
                "created_at": 1787443200000_u64
            },
            "created_at": 1787443200000_u64
        })),
    )
}

async fn capture_snapshot_name_update_request(
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
    let mut snapshot = snapshot_response()
        .as_array()
        .and_then(|snapshots| snapshots.first())
        .cloned()
        .unwrap();
    snapshot["name"] = json!("Renamed snapshot");
    Json(snapshot)
}

async fn capture_sandbox_list_request(
    State(request_tx): State<mpsc::UnboundedSender<String>>,
    headers: HeaderMap,
) -> Json<Value> {
    let authorization = headers
        .get("authorization")
        .and_then(|value| value.to_str().ok())
        .unwrap_or_default()
        .to_owned();
    request_tx.send(authorization).unwrap();
    Json(json!([{
        "machine_id": "machine-e2b-1",
        "sandbox_account_id": E2B_ACCOUNT_ID,
        "sandbox_id": "e2b-sandbox-1",
        "name": "Development",
        "created_from": "base",
        "created_at": 1787443200000_u64
    }]))
}

async fn capture_sandbox_create_request(
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
            "machine": {
                "machine_id": "machine-e2b-1",
                "name": "Development",
                "connector": "sandbox",
                "online": true,
                "descriptor": {
                    "protocol_version": {"major": 1, "minor": 0},
                    "machine_id": "machine-e2b-1",
                    "name": "Development",
                    "operating_system": {"type": "linux"},
                    "architecture": "x86_64",
                    "path_convention": "posix",
                    "workspace_roots": [],
                    "capabilities": []
                },
                "created_at": 1787443200000_u64,
                "updated_at": 1787443200000_u64
            },
            "sandbox_account_id": E2B_ACCOUNT_ID,
            "sandbox_id": "e2b-sandbox-1",
            "created_from": "base"
        })),
    )
}

async fn capture_snapshot_list_request(
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
    Json(snapshot_response())
}

async fn capture_sandbox_snapshot_create_request(
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
    let snapshot = snapshot_response()
        .as_array()
        .and_then(|snapshots| snapshots.first())
        .cloned()
        .unwrap();
    (StatusCode::CREATED, Json(snapshot))
}

fn snapshot_response() -> Value {
    json!([{
        "id": "01992aa0-0000-7000-8000-000000000020",
        "name": "Ready workspace",
        "sandbox_account_id": E2B_ACCOUNT_ID,
        "provider": "e2b",
        "provider_snapshot_id": "snapshot-1",
        "sandbox_id": "e2b-sandbox-1",
        "created_at": 1787443200000_u64
    }])
}
