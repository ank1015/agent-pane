use std::{
    collections::BTreeMap,
    sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    },
    time::Duration,
};

use axum::{
    Json, Router,
    extract::{Path, Query, State},
    http::{HeaderMap, StatusCode},
    routing::{get, post},
};
use execution_contracts::{
    Capability, ExecutionError, ExecutionErrorCode, ExecutionId, FileKind, FileMetadata,
    InspectRequest, MachineDescriptor, MachineId, OperatingSystem, PROCESS_SESSION_CAPABILITY,
    PathConvention, PathSpec, ProcessEvent, ProcessEventKind, ProtocolVersion, TimestampMs,
    WORKSPACE_QUERY_CAPABILITY, WorkspaceRoot,
};
use execution_gateway_client::{ExecutionGatewayClient, ExecutionGatewayConfig};
use execution_protocol::{ConnectorKind, MachineSummary};
use execution_runtime::{ExecutionRuntime, OperationContext};
use futures_util::StreamExt;
use serde::Deserialize;
use serde_json::{Value, json};
use tokio::sync::{Mutex, Notify};

const OPERATION_ID: &str = "019d2aa0-0000-7000-8000-000000000001";

#[derive(Clone, Copy)]
enum Mode {
    Unary,
    Stream,
    Running,
    Reject,
}

#[derive(Clone)]
struct MockState {
    mode: Mode,
    descriptor: MachineDescriptor,
    operation: Arc<Mutex<Option<Value>>>,
    operation_target: Arc<Mutex<Option<String>>>,
    authorization: Arc<Mutex<Option<String>>>,
    polled: Arc<Notify>,
    cancellations: Arc<AtomicUsize>,
}

#[tokio::test]
async fn environment_runtime_uses_the_saved_location_and_environment_operation_route() {
    let (client, state) = mock_client(Mode::Unary).await;
    let environment = client
        .environment(&id("environment-1"))
        .await
        .expect("environment connects");

    assert_eq!(environment.environment_id().as_str(), "environment-1");
    assert_eq!(environment.machine_id().as_str(), "machine-1");
    assert_eq!(environment.workspace_root_id().as_str(), "root");
    assert_eq!(environment.path(), "project");

    environment
        .workspace_query()
        .inspect(&OperationContext::new(), inspect_request())
        .await
        .expect("environment operation succeeds");

    assert_eq!(
        state.operation_target.lock().await.as_deref(),
        Some("environment-1")
    );
}

#[tokio::test]
async fn machine_runtime_executes_a_typed_unary_operation() {
    let (client, state) = mock_client(Mode::Unary).await;
    let machine_id = id::<MachineId>("machine-1");
    let runtime = client
        .machine_runtime(&machine_id)
        .await
        .expect("runtime connects");
    let request = inspect_request();

    let metadata = runtime
        .workspace_query()
        .inspect(&OperationContext::new(), request.clone())
        .await
        .expect("inspect succeeds");

    assert_eq!(metadata, file_metadata());
    assert_eq!(
        state.authorization.lock().await.as_deref(),
        Some("Bearer api-token")
    );
    assert_eq!(
        state.operation.lock().await.as_ref(),
        Some(&json!({
            "operation": "workspace_inspect",
            "request": request
        }))
    );
}

#[tokio::test]
async fn process_attach_exposes_persisted_gateway_events_as_a_stream() {
    let (client, _) = mock_client(Mode::Stream).await;
    let runtime = client
        .machine_runtime(&id("machine-1"))
        .await
        .expect("runtime connects");
    let process = runtime
        .process_runtime()
        .expect("process capability is advertised");
    let mut stream = process
        .attach(
            &OperationContext::new(),
            execution_contracts::AttachExecutionRequest {
                execution_id: id("execution-1"),
                after_sequence: None,
            },
        )
        .await
        .expect("attach starts");

    let first = stream.next().await.expect("first event").expect("event");
    let second = stream.next().await.expect("second event").expect("event");

    assert_eq!(first.sequence, 1);
    assert_eq!(second.sequence, 2);
    assert!(stream.next().await.is_none());
}

#[tokio::test]
async fn context_cancellation_cancels_the_gateway_operation() {
    let (client, state) = mock_client(Mode::Running).await;
    let runtime = client
        .machine_runtime(&id("machine-1"))
        .await
        .expect("runtime connects");
    let context = OperationContext::new();
    let task_context = context.clone();
    let task = tokio::spawn(async move {
        runtime
            .workspace_query()
            .inspect(&task_context, inspect_request())
            .await
    });

    state.polled.notified().await;
    context.cancel();
    let error = task.await.expect("task joins").expect_err("cancelled");

    assert_eq!(error.code, ExecutionErrorCode::Cancelled);
    assert_eq!(state.cancellations.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn gateway_execution_errors_pass_through_the_runtime_interface() {
    let (client, _) = mock_client(Mode::Reject).await;
    let runtime = client
        .machine_runtime(&id("machine-1"))
        .await
        .expect("runtime connects");

    let error = runtime
        .workspace_query()
        .inspect(&OperationContext::new(), inspect_request())
        .await
        .expect_err("gateway rejection is returned");

    assert_eq!(error.code, ExecutionErrorCode::PermissionDenied);
    assert_eq!(error.message, "operation denied");
}

async fn mock_client(mode: Mode) -> (ExecutionGatewayClient, MockState) {
    let state = MockState {
        mode,
        descriptor: descriptor(),
        operation: Arc::new(Mutex::new(None)),
        operation_target: Arc::new(Mutex::new(None)),
        authorization: Arc::new(Mutex::new(None)),
        polled: Arc::new(Notify::new()),
        cancellations: Arc::new(AtomicUsize::new(0)),
    };
    let app = Router::new()
        .route("/v1/machines/{machine_id}", get(get_machine))
        .route("/v1/environments/{environment_id}", get(get_environment))
        .route(
            "/v1/machines/{machine_id}/operations",
            post(create_operation),
        )
        .route(
            "/v1/environments/{environment_id}/operations",
            post(create_environment_operation),
        )
        .route("/v1/operations/{operation_id}", get(get_operation))
        .route("/v1/operations/{operation_id}/events", get(get_events))
        .route(
            "/v1/operations/{operation_id}/cancel",
            post(cancel_operation),
        )
        .with_state(state.clone());
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind mock gateway");
    let address = listener.local_addr().expect("mock address");
    tokio::spawn(async move {
        axum::serve(listener, app)
            .await
            .expect("serve mock gateway");
    });

    let mut config = ExecutionGatewayConfig::new(
        format!("http://{address}").parse().expect("valid URL"),
        "api-token",
    );
    config.poll_interval = Duration::from_millis(1);
    config.request_timeout = Duration::from_secs(2);
    (
        ExecutionGatewayClient::new(config).expect("build gateway client"),
        state,
    )
}

async fn get_machine(
    State(state): State<MockState>,
    Path(machine_id): Path<String>,
) -> Json<Value> {
    assert_eq!(machine_id, "machine-1");
    Json(
        serde_json::to_value(MachineSummary {
            machine_id: id("machine-1"),
            name: "Test machine".to_owned(),
            connector: ConnectorKind::MachineDaemon,
            online: true,
            descriptor: state.descriptor,
            created_at: TimestampMs(1),
            updated_at: TimestampMs(1),
            last_seen_at: Some(TimestampMs(1)),
        })
        .expect("machine serializes"),
    )
}

async fn get_environment(Path(environment_id): Path<String>) -> Json<Value> {
    assert_eq!(environment_id, "environment-1");
    Json(json!({
        "environment_id": "environment-1",
        "machine_id": "machine-1",
        "workspace_root_id": "root",
        "path": "project",
        "created_at": 1
    }))
}

async fn create_operation(
    State(state): State<MockState>,
    Path(machine_id): Path<String>,
    headers: HeaderMap,
    Json(body): Json<Value>,
) -> (StatusCode, Json<Value>) {
    assert_eq!(machine_id, "machine-1");
    *state.operation_target.lock().await = Some(machine_id);
    *state.authorization.lock().await = headers
        .get("authorization")
        .and_then(|value| value.to_str().ok())
        .map(ToOwned::to_owned);
    let operation = body["operation"].clone();
    *state.operation.lock().await = Some(operation.clone());
    if matches!(state.mode, Mode::Reject) {
        return (
            StatusCode::FORBIDDEN,
            Json(
                serde_json::to_value(execution_error(
                    ExecutionErrorCode::PermissionDenied,
                    "operation denied",
                ))
                .expect("error serializes"),
            ),
        );
    }
    (
        StatusCode::ACCEPTED,
        Json(operation_record("queued", operation, None, None)),
    )
}

async fn create_environment_operation(
    State(state): State<MockState>,
    Path(environment_id): Path<String>,
    headers: HeaderMap,
    Json(body): Json<Value>,
) -> (StatusCode, Json<Value>) {
    assert_eq!(environment_id, "environment-1");
    *state.operation_target.lock().await = Some(environment_id);
    *state.authorization.lock().await = headers
        .get("authorization")
        .and_then(|value| value.to_str().ok())
        .map(ToOwned::to_owned);
    let operation = body["operation"].clone();
    *state.operation.lock().await = Some(operation.clone());
    let mut record = operation_record("queued", operation, None, None);
    record["environment_id"] = json!("environment-1");
    (StatusCode::ACCEPTED, Json(record))
}

async fn get_operation(State(state): State<MockState>) -> Json<Value> {
    state.polled.notify_one();
    let operation = state
        .operation
        .lock()
        .await
        .clone()
        .unwrap_or_else(|| json!({"operation": "describe"}));
    let value = match state.mode {
        Mode::Unary => operation_record(
            "completed",
            operation,
            Some(json!({"result": "file_metadata", "value": file_metadata()})),
            None,
        ),
        Mode::Stream => operation_record("completed", operation, None, None),
        Mode::Running | Mode::Reject => operation_record("running", operation, None, None),
    };
    Json(value)
}

#[derive(Deserialize)]
struct EventsQuery {
    after: u64,
}

async fn get_events(
    State(state): State<MockState>,
    Query(query): Query<EventsQuery>,
) -> Json<Value> {
    if !matches!(state.mode, Mode::Stream) || query.after > 0 {
        return Json(json!([]));
    }
    Json(json!([
        {"sequence": 1, "item": {"item": "process_event", "value": process_event(1)}, "created_at": 1},
        {"sequence": 2, "item": {"item": "process_event", "value": process_event(2)}, "created_at": 1}
    ]))
}

async fn cancel_operation(State(state): State<MockState>) -> Json<Value> {
    state.cancellations.fetch_add(1, Ordering::SeqCst);
    let operation = state
        .operation
        .lock()
        .await
        .clone()
        .unwrap_or_else(|| json!({"operation": "describe"}));
    Json(operation_record("cancelled", operation, None, None))
}

fn operation_record(
    status: &str,
    operation: Value,
    response: Option<Value>,
    error: Option<Value>,
) -> Value {
    let mut value = json!({
        "operation_id": OPERATION_ID,
        "machine_id": "machine-1",
        "status": status,
        "operation": operation,
        "created_at": 1,
        "updated_at": 1
    });
    if let Some(response) = response {
        value["response"] = response;
    }
    if let Some(error) = error {
        value["error"] = error;
    }
    value
}

fn descriptor() -> MachineDescriptor {
    MachineDescriptor {
        protocol_version: ProtocolVersion::V1,
        machine_id: id("machine-1"),
        name: "Test machine".to_owned(),
        operating_system: OperatingSystem::Linux,
        architecture: "x86_64".to_owned(),
        path_convention: PathConvention::Posix,
        default_shell: None,
        workspace_roots: vec![WorkspaceRoot {
            id: id("root"),
            name: "Workspace".to_owned(),
            uri: "file:///workspace".to_owned(),
            read_only: false,
        }],
        capabilities: [WORKSPACE_QUERY_CAPABILITY, PROCESS_SESSION_CAPABILITY]
            .into_iter()
            .map(|capability| Capability::v1(capability).expect("valid capability"))
            .collect(),
    }
}

fn inspect_request() -> InspectRequest {
    InspectRequest {
        path: PathSpec::workspace(id("root"), "src/lib.rs"),
        follow_symlinks: true,
    }
}

fn file_metadata() -> FileMetadata {
    FileMetadata {
        path: PathSpec::workspace(id("root"), "src/lib.rs"),
        kind: FileKind::File,
        size: 42,
        created_at: None,
        modified_at: None,
        revision: None,
        mime_type: Some("text/plain".to_owned()),
    }
}

fn process_event(sequence: u64) -> ProcessEvent {
    ProcessEvent {
        execution_id: id::<ExecutionId>("execution-1"),
        sequence,
        timestamp: TimestampMs(1),
        event: if sequence == 1 {
            ProcessEventKind::Started
        } else {
            ProcessEventKind::Closed
        },
    }
}

fn execution_error(code: ExecutionErrorCode, message: &str) -> ExecutionError {
    ExecutionError {
        code,
        message: message.to_owned(),
        retryable: false,
        details: BTreeMap::new(),
    }
}

fn id<T>(value: &str) -> T
where
    T: std::str::FromStr,
    T::Err: std::fmt::Debug,
{
    value.parse().expect("valid identifier")
}
