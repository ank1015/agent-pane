use axum::{
    Json, Router,
    extract::{Path, State, rejection::JsonRejection},
    http::{HeaderValue, StatusCode, header::CACHE_CONTROL},
    middleware,
    response::{IntoResponse, Response},
    routing::{get, put},
};
use uuid::Uuid;

use super::{
    MachineService,
    model::{
        CreateSandboxAccountRequest, MachineInventory, MachineResponse,
        RotateSandboxCredentialsRequest, SandboxAccount, SandboxAccountResponse, UpdateNameRequest,
    },
};
use crate::{AppState, error::ApiError};

#[derive(Clone)]
struct MachineState {
    service: MachineService,
}

pub(super) fn router(service: MachineService) -> Router<AppState> {
    Router::new()
        .route("/api/machines", get(machine_inventory))
        .route(
            "/api/machines/sandbox-accounts",
            get(list_sandbox_accounts).post(create_sandbox_account),
        )
        .route(
            "/api/machines/sandbox-accounts/{account_id}",
            get(get_sandbox_account)
                .patch(update_sandbox_account_name)
                .delete(delete_sandbox_account),
        )
        .route(
            "/api/machines/sandbox-accounts/{account_id}/credentials",
            put(rotate_sandbox_credentials),
        )
        .route(
            "/api/machines/tunnels/{machine_id}",
            axum::routing::patch(update_machine_name).delete(delete_machine),
        )
        .layer(middleware::map_response(add_no_store))
        .with_state(MachineState { service })
}

async fn machine_inventory(
    State(state): State<MachineState>,
) -> Result<Json<MachineInventory>, ApiError> {
    Ok(Json(state.service.machine_inventory().await?))
}

async fn list_sandbox_accounts(
    State(state): State<MachineState>,
) -> Result<Json<Vec<SandboxAccount>>, ApiError> {
    Ok(Json(state.service.list_sandbox_accounts().await?))
}

async fn get_sandbox_account(
    State(state): State<MachineState>,
    Path(account_id): Path<Uuid>,
) -> Result<Json<SandboxAccountResponse>, ApiError> {
    let account = state.service.get_sandbox_account(account_id).await?;
    Ok(Json(SandboxAccountResponse { account }))
}

async fn create_sandbox_account(
    State(state): State<MachineState>,
    payload: Result<Json<CreateSandboxAccountRequest>, JsonRejection>,
) -> Result<Response, ApiError> {
    let Json(request) = payload.map_err(|error| ApiError::invalid_request(error.body_text()))?;
    let account = state.service.create_sandbox_account(&request).await?;
    Ok((
        StatusCode::CREATED,
        Json(SandboxAccountResponse { account }),
    )
        .into_response())
}

async fn rotate_sandbox_credentials(
    State(state): State<MachineState>,
    Path(account_id): Path<Uuid>,
    payload: Result<Json<RotateSandboxCredentialsRequest>, JsonRejection>,
) -> Result<Json<SandboxAccountResponse>, ApiError> {
    let Json(request) = payload.map_err(|error| ApiError::invalid_request(error.body_text()))?;
    let account = state
        .service
        .rotate_sandbox_credentials(account_id, &request)
        .await?;
    Ok(Json(SandboxAccountResponse { account }))
}

async fn update_sandbox_account_name(
    State(state): State<MachineState>,
    Path(account_id): Path<Uuid>,
    payload: Result<Json<UpdateNameRequest>, JsonRejection>,
) -> Result<Json<SandboxAccountResponse>, ApiError> {
    let Json(request) = payload.map_err(|error| ApiError::invalid_request(error.body_text()))?;
    let account = state
        .service
        .update_sandbox_account_name(account_id, &request)
        .await?;
    Ok(Json(SandboxAccountResponse { account }))
}

async fn delete_sandbox_account(
    State(state): State<MachineState>,
    Path(account_id): Path<Uuid>,
) -> Result<StatusCode, ApiError> {
    state.service.delete_sandbox_account(account_id).await?;
    Ok(StatusCode::NO_CONTENT)
}

async fn update_machine_name(
    State(state): State<MachineState>,
    Path(machine_id): Path<String>,
    payload: Result<Json<UpdateNameRequest>, JsonRejection>,
) -> Result<Json<MachineResponse>, ApiError> {
    let Json(request) = payload.map_err(|error| ApiError::invalid_request(error.body_text()))?;
    let machine = state
        .service
        .update_machine_name(&machine_id, &request)
        .await?;
    Ok(Json(MachineResponse { machine }))
}

async fn delete_machine(
    State(state): State<MachineState>,
    Path(machine_id): Path<String>,
) -> Result<StatusCode, ApiError> {
    state.service.delete_machine(&machine_id).await?;
    Ok(StatusCode::NO_CONTENT)
}

async fn add_no_store(mut response: Response) -> Response {
    response
        .headers_mut()
        .insert(CACHE_CONTROL, HeaderValue::from_static("no-store"));
    response
}
