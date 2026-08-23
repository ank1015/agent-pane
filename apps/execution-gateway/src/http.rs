use std::{sync::Arc, time::Duration};

use axum::{
    Json, Router,
    extract::{DefaultBodyLimit, Path, Query, State, WebSocketUpgrade},
    http::{HeaderMap, StatusCode},
    response::{IntoResponse, Response as HttpResponse},
    routing::{get, post, put},
};
use chrono::Utc;
use execution_contracts::{ExecutionErrorCode, Validate};
use execution_protocol::{
    ClaimMachineRequest, ClaimMachineResponse, ConnectorKind, CreateOperationRequest,
    CreateRegistrationRequest, MachineSummary, OperationEvent, OperationRecord,
    RegistrationCreated,
};
use serde::{Deserialize, Serialize};
use subtle::ConstantTimeEq;
use uuid::Uuid;

use crate::{
    authentication,
    config::Config,
    connection_registry::{ConnectionRegistry, execution_error},
    db::Database,
    routing::OperationRouter,
    sandbox_accounts::{
        CreateSandboxAccountRequest, RotateSandboxCredentialsRequest, SandboxAccount,
        SandboxAccountError, SandboxAccountService, UpdateSandboxAccountNameRequest,
    },
    token,
};

#[derive(Clone)]
pub struct AppState {
    database: Database,
    connections: ConnectionRegistry,
    router: OperationRouter,
    admin_token: Arc<str>,
    api_token: Arc<str>,
    control_token: Arc<str>,
    sandbox_accounts: SandboxAccountService,
    daemon_websocket_url: Arc<str>,
    registration_ttl: Duration,
}

pub fn router(
    config: &Config,
    database: Database,
    connections: ConnectionRegistry,
    operation_router: OperationRouter,
) -> Router {
    let state = AppState {
        database: database.clone(),
        connections,
        router: operation_router,
        admin_token: Arc::from(config.admin_token.as_str()),
        api_token: Arc::from(config.api_token.as_str()),
        control_token: Arc::from(config.control_token.as_str()),
        sandbox_accounts: SandboxAccountService::new(database.clone(), config.vault.clone()),
        daemon_websocket_url: Arc::from(config.daemon_websocket_url.as_str()),
        registration_ttl: config.registration_ttl,
    };
    Router::new()
        .route("/health", get(health))
        .route("/v1/admin/machine-registrations", post(create_registration))
        .route("/v1/machine-registrations/claim", post(claim_registration))
        .route("/v1/machines/connect", get(connect_machine))
        .route(
            "/v1/control/sandbox-accounts",
            get(list_sandbox_accounts).post(create_sandbox_account),
        )
        .route(
            "/v1/control/sandbox-accounts/{account_id}",
            get(get_sandbox_account)
                .patch(update_sandbox_account_name)
                .delete(delete_sandbox_account),
        )
        .route(
            "/v1/control/sandbox-accounts/{account_id}/credentials",
            put(rotate_sandbox_credentials),
        )
        .route("/v1/control/machines", get(machine_inventory))
        .route(
            "/v1/control/machines/{machine_id}",
            axum::routing::patch(update_machine_name).delete(delete_machine),
        )
        .route("/v1/machines", get(list_machines))
        .route("/v1/machines/{machine_id}", get(get_machine))
        .route(
            "/v1/machines/{machine_id}/operations",
            post(create_operation),
        )
        .route("/v1/operations/{operation_id}", get(get_operation))
        .route(
            "/v1/operations/{operation_id}/cancel",
            post(cancel_operation),
        )
        .route("/v1/operations/{operation_id}/events", get(get_events))
        .layer(DefaultBodyLimit::max(config.max_request_bytes))
        .with_state(state)
}

#[derive(Serialize)]
struct Health {
    status: &'static str,
}

async fn health(State(state): State<AppState>) -> Result<Json<Health>, ApiError> {
    state.database.health().await.map_err(ApiError::database)?;
    Ok(Json(Health { status: "ok" }))
}

async fn create_registration(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(request): Json<CreateRegistrationRequest>,
) -> Result<(StatusCode, Json<RegistrationCreated>), ApiError> {
    require(&headers, &state.admin_token)?;
    let ttl = request
        .expires_in_seconds
        .map(Duration::from_secs)
        .unwrap_or(state.registration_ttl)
        .clamp(Duration::from_secs(60), Duration::from_secs(24 * 60 * 60));
    let expires =
        Utc::now() + chrono::Duration::from_std(ttl).map_err(|e| ApiError::bad(e.to_string()))?;
    let registration_token = token::generate();
    let id = state
        .database
        .create_registration(
            &token::hash(&registration_token),
            request.label.as_deref(),
            expires,
        )
        .await
        .map_err(ApiError::database)?;
    Ok((
        StatusCode::CREATED,
        Json(RegistrationCreated {
            registration_id: id.to_string(),
            registration_token,
            expires_at: execution_contracts::TimestampMs(
                u64::try_from(expires.timestamp_millis()).unwrap_or(0),
            ),
        }),
    ))
}

async fn claim_registration(
    State(state): State<AppState>,
    Json(request): Json<ClaimMachineRequest>,
) -> Result<Json<ClaimMachineResponse>, ApiError> {
    request
        .descriptor
        .validate()
        .map_err(|e| ApiError::bad(e.to_string()))?;
    let credential = token::generate();
    let claimed = state
        .database
        .claim_registration(
            &token::hash(&request.registration_token),
            &token::hash(&credential),
            &request.descriptor,
        )
        .await
        .map_err(ApiError::database)?;
    if !claimed {
        return Err(ApiError::new(
            StatusCode::UNAUTHORIZED,
            "invalid, expired, or already claimed registration token",
        ));
    }
    Ok(Json(ClaimMachineResponse {
        machine_id: request.descriptor.machine_id,
        credential,
        websocket_url: state.daemon_websocket_url.to_string(),
    }))
}

async fn connect_machine(
    State(state): State<AppState>,
    headers: HeaderMap,
    upgrade: WebSocketUpgrade,
) -> Result<HttpResponse, ApiError> {
    let machine_id = headers
        .get("x-machine-id")
        .and_then(|v| v.to_str().ok())
        .ok_or_else(|| ApiError::new(StatusCode::UNAUTHORIZED, "missing x-machine-id"))?
        .to_owned();
    let credential = authentication::bearer_value(&headers)
        .ok_or_else(|| ApiError::new(StatusCode::UNAUTHORIZED, "missing machine credential"))?;
    let machine = state
        .database
        .machine(&machine_id)
        .await
        .map_err(ApiError::database)?
        .ok_or_else(|| ApiError::new(StatusCode::UNAUTHORIZED, "machine is not registered"))?;
    let expected = machine.credential_hash.ok_or_else(|| {
        ApiError::new(StatusCode::UNAUTHORIZED, "machine has no daemon credential")
    })?;
    if !bool::from(token::hash(credential).ct_eq(&expected)) {
        return Err(ApiError::new(
            StatusCode::UNAUTHORIZED,
            "invalid machine credential",
        ));
    }
    let registry = state.connections.clone();
    let database = state.database.clone();
    Ok(upgrade
        .on_upgrade(move |socket| async move {
            if let Err(source) = registry.accept(machine_id, socket, database).await {
                tracing::warn!(%source, "machine connection ended with error");
            }
        })
        .into_response())
}

async fn list_sandbox_accounts(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> Result<Json<Vec<SandboxAccount>>, ApiError> {
    require(&headers, &state.control_token)?;
    state
        .sandbox_accounts
        .list()
        .await
        .map(Json)
        .map_err(ApiError::sandbox_account)
}

async fn create_sandbox_account(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(request): Json<CreateSandboxAccountRequest>,
) -> Result<(StatusCode, Json<SandboxAccount>), ApiError> {
    require(&headers, &state.control_token)?;
    state
        .sandbox_accounts
        .create(request)
        .await
        .map(|account| (StatusCode::CREATED, Json(account)))
        .map_err(ApiError::sandbox_account)
}

async fn get_sandbox_account(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(account_id): Path<Uuid>,
) -> Result<Json<SandboxAccount>, ApiError> {
    require(&headers, &state.control_token)?;
    state
        .sandbox_accounts
        .find(account_id)
        .await
        .map_err(ApiError::sandbox_account)?
        .map(Json)
        .ok_or_else(ApiError::not_found)
}

async fn rotate_sandbox_credentials(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(account_id): Path<Uuid>,
    Json(request): Json<RotateSandboxCredentialsRequest>,
) -> Result<Json<SandboxAccount>, ApiError> {
    require(&headers, &state.control_token)?;
    state
        .sandbox_accounts
        .rotate_credentials(account_id, request)
        .await
        .map_err(ApiError::sandbox_account)?
        .map(Json)
        .ok_or_else(ApiError::not_found)
}

async fn update_sandbox_account_name(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(account_id): Path<Uuid>,
    Json(request): Json<UpdateSandboxAccountNameRequest>,
) -> Result<Json<SandboxAccount>, ApiError> {
    require(&headers, &state.control_token)?;
    state
        .sandbox_accounts
        .update_name(account_id, request)
        .await
        .map_err(ApiError::sandbox_account)?
        .map(Json)
        .ok_or_else(ApiError::not_found)
}

async fn delete_sandbox_account(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(account_id): Path<Uuid>,
) -> Result<StatusCode, ApiError> {
    require(&headers, &state.control_token)?;
    state
        .sandbox_accounts
        .delete(account_id)
        .await
        .map_err(ApiError::sandbox_account)?
        .then_some(StatusCode::NO_CONTENT)
        .ok_or_else(ApiError::not_found)
}

#[derive(Serialize)]
struct MachineInventory {
    connector_accounts: Vec<SandboxAccount>,
    machine_daemons: Vec<MachineSummary>,
}

async fn machine_inventory(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> Result<Json<MachineInventory>, ApiError> {
    require(&headers, &state.control_token)?;
    let (connector_accounts, machine_daemons) = tokio::join!(
        state.sandbox_accounts.list(),
        machine_summaries(&state, Some(ConnectorKind::MachineDaemon)),
    );
    Ok(Json(MachineInventory {
        connector_accounts: connector_accounts.map_err(ApiError::sandbox_account)?,
        machine_daemons: machine_daemons?,
    }))
}

#[derive(Deserialize)]
struct UpdateMachineNameRequest {
    name: String,
}

async fn update_machine_name(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(machine_id): Path<String>,
    Json(request): Json<UpdateMachineNameRequest>,
) -> Result<Json<MachineSummary>, ApiError> {
    require(&headers, &state.control_token)?;
    validate_machine_name(&request.name)?;
    let mut machine = state
        .database
        .update_machine_name(&machine_id, &request.name)
        .await
        .map_err(ApiError::database)?
        .ok_or_else(ApiError::not_found)?;
    machine.summary.online = state.connections.online(&machine_id).await;
    Ok(Json(machine.summary))
}

async fn delete_machine(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(machine_id): Path<String>,
) -> Result<StatusCode, ApiError> {
    require(&headers, &state.control_token)?;
    if !state
        .database
        .delete_machine(&machine_id)
        .await
        .map_err(ApiError::database)?
    {
        return Err(ApiError::not_found());
    }
    state.connections.disconnect(&machine_id).await;
    Ok(StatusCode::NO_CONTENT)
}

async fn list_machines(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> Result<Json<Vec<MachineSummary>>, ApiError> {
    require(&headers, &state.api_token)?;
    machine_summaries(&state, None).await.map(Json)
}

async fn machine_summaries(
    state: &AppState,
    connector: Option<ConnectorKind>,
) -> Result<Vec<MachineSummary>, ApiError> {
    let mut machines = Vec::new();
    for mut machine in state
        .database
        .machines()
        .await
        .map_err(ApiError::database)?
    {
        if connector.is_some_and(|connector| machine.summary.connector != connector) {
            continue;
        }
        machine.summary.online = state
            .connections
            .online(machine.summary.machine_id.as_str())
            .await;
        machines.push(machine.summary);
    }
    Ok(machines)
}

async fn get_machine(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(machine_id): Path<String>,
) -> Result<Json<MachineSummary>, ApiError> {
    require(&headers, &state.api_token)?;
    let mut machine = state
        .database
        .machine(&machine_id)
        .await
        .map_err(ApiError::database)?
        .ok_or_else(ApiError::not_found)?;
    machine.summary.online = state.connections.online(&machine_id).await;
    Ok(Json(machine.summary))
}

async fn create_operation(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(machine_id): Path<String>,
    Json(request): Json<CreateOperationRequest>,
) -> Result<(StatusCode, Json<OperationRecord>), ApiError> {
    require(&headers, &state.api_token)?;
    let machine = state
        .database
        .machine(&machine_id)
        .await
        .map_err(ApiError::database)?
        .ok_or_else(ApiError::not_found)?;
    let record = state
        .database
        .create_operation(&machine_id, &request.operation)
        .await
        .map_err(ApiError::database)?;
    let id = Uuid::parse_str(&record.operation_id).map_err(|e| ApiError::bad(e.to_string()))?;
    state.router.spawn(id, machine, request.operation);
    Ok((StatusCode::ACCEPTED, Json(record)))
}

async fn get_operation(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(operation_id): Path<Uuid>,
) -> Result<Json<OperationRecord>, ApiError> {
    require(&headers, &state.api_token)?;
    state
        .database
        .operation(operation_id)
        .await
        .map_err(ApiError::database)?
        .map(Json)
        .ok_or_else(ApiError::not_found)
}

async fn cancel_operation(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(operation_id): Path<Uuid>,
) -> Result<Json<OperationRecord>, ApiError> {
    require(&headers, &state.api_token)?;
    let record = state
        .database
        .cancel(operation_id)
        .await
        .map_err(ApiError::database)?
        .ok_or_else(ApiError::not_found)?;
    let machine = state
        .database
        .machine(record.machine_id.as_str())
        .await
        .map_err(ApiError::database)?
        .ok_or_else(ApiError::not_found)?;
    state.router.cancel(operation_id, &machine).await;
    Ok(Json(record))
}

#[derive(Deserialize)]
struct EventsQuery {
    #[serde(default)]
    after: u64,
    #[serde(default = "default_limit")]
    limit: u32,
}
fn default_limit() -> u32 {
    100
}

async fn get_events(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(operation_id): Path<Uuid>,
    Query(query): Query<EventsQuery>,
) -> Result<Json<Vec<OperationEvent>>, ApiError> {
    require(&headers, &state.api_token)?;
    Ok(Json(
        state
            .database
            .events(operation_id, query.after, query.limit.clamp(1, 1000))
            .await
            .map_err(ApiError::database)?,
    ))
}

fn require(headers: &HeaderMap, token: &str) -> Result<(), ApiError> {
    authentication::bearer(headers, token)
        .then_some(())
        .ok_or_else(|| ApiError::new(StatusCode::UNAUTHORIZED, "invalid bearer token"))
}

fn validate_machine_name(name: &str) -> Result<(), ApiError> {
    if name.is_empty() || name != name.trim() || name.chars().count() > 120 {
        return Err(ApiError::bad(
            "machine name must be 1-120 characters without surrounding whitespace",
        ));
    }
    Ok(())
}

pub struct ApiError {
    status: StatusCode,
    error: execution_contracts::ExecutionError,
}
impl ApiError {
    fn new(status: StatusCode, message: impl Into<String>) -> Self {
        Self {
            status,
            error: execution_error(ExecutionErrorCode::InvalidRequest, message, false),
        }
    }
    fn bad(message: impl Into<String>) -> Self {
        Self::new(StatusCode::BAD_REQUEST, message)
    }
    fn not_found() -> Self {
        Self {
            status: StatusCode::NOT_FOUND,
            error: execution_error(ExecutionErrorCode::NotFound, "resource not found", false),
        }
    }
    fn sandbox_account(error: SandboxAccountError) -> Self {
        match error {
            SandboxAccountError::InvalidName
            | SandboxAccountError::ConfigMustBeObject
            | SandboxAccountError::InvalidApiKey
            | SandboxAccountError::DisabledDefault => Self::bad(error.to_string()),
            SandboxAccountError::NameConflict => Self {
                status: StatusCode::CONFLICT,
                error: execution_error(ExecutionErrorCode::AlreadyExists, error.to_string(), false),
            },
            SandboxAccountError::Database(crate::db::DbError::Contract(message))
                if message == "sandbox account owns active machines" =>
            {
                Self {
                    status: StatusCode::CONFLICT,
                    error: execution_error(ExecutionErrorCode::Conflict, message, false),
                }
            }
            internal => {
                tracing::error!(error=%internal, "sandbox account operation failed");
                Self {
                    status: StatusCode::INTERNAL_SERVER_ERROR,
                    error: execution_error(
                        ExecutionErrorCode::Internal,
                        "internal sandbox account error",
                        true,
                    ),
                }
            }
        }
    }
    fn database(error: impl std::fmt::Display) -> Self {
        tracing::error!(%error, "database error");
        Self {
            status: StatusCode::INTERNAL_SERVER_ERROR,
            error: execution_error(
                ExecutionErrorCode::Internal,
                "internal database error",
                true,
            ),
        }
    }
}
impl IntoResponse for ApiError {
    fn into_response(self) -> HttpResponse {
        (self.status, Json(self.error)).into_response()
    }
}
