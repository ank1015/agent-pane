use axum::{
    Json, Router,
    extract::{Path, Query, State, rejection::JsonRejection},
    http::{HeaderValue, StatusCode, header::CACHE_CONTROL},
    middleware,
    response::{IntoResponse, Response},
    routing::{get, put},
};
use execution_protocol::Environment;
use uuid::Uuid;

use super::{
    MachineService,
    model::{
        CreateMachineEnvironmentRequest, CreateSandboxAccountRequest,
        CreateSandboxEnvironmentTemplateRequest, CreateSandboxRequest,
        CreateSandboxSnapshotRequest, CreateSnapshotRequest, MachineInventory, MachineResponse,
        RotateSandboxCredentialsRequest, SandboxAccount, SandboxAccountResponse, SandboxCreated,
        SandboxEnvironmentInstance, SandboxEnvironmentTemplate, SandboxMachine, Snapshot,
        SnapshotQuery, UpdateNameRequest, UpdateSandboxEnvironmentTemplateRequest,
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
            "/api/machines/{machine_id}/environments",
            get(list_machine_environments).post(create_machine_environment),
        )
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
            "/api/machines/sandbox-accounts/{account_id}/sandboxes",
            get(list_sandbox_machines).post(create_sandbox),
        )
        .route(
            "/api/machines/sandbox-accounts/{account_id}/sandboxes/{sandbox_id}/snapshots",
            axum::routing::post(create_sandbox_snapshot),
        )
        .route(
            "/api/machines/snapshots",
            get(list_snapshots).post(create_snapshot),
        )
        .route(
            "/api/machines/snapshots/{snapshot_id}",
            get(get_snapshot)
                .patch(update_snapshot_name)
                .delete(delete_snapshot),
        )
        .route(
            "/api/machines/sandbox-environment-templates",
            get(list_sandbox_environment_templates).post(create_sandbox_environment_template),
        )
        .route(
            "/api/machines/sandbox-environment-templates/{template_id}",
            get(get_sandbox_environment_template)
                .patch(update_sandbox_environment_template)
                .delete(delete_sandbox_environment_template),
        )
        .route(
            "/api/machines/sandbox-environment-templates/{template_id}/environments",
            get(list_sandbox_environment_instances).post(materialize_sandbox_environment_template),
        )
        .route(
            "/api/machines/environments/{environment_id}",
            axum::routing::patch(update_environment_name).delete(delete_environment),
        )
        .route(
            "/api/machines/{machine_id}",
            axum::routing::patch(update_machine_name),
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

async fn list_machine_environments(
    State(state): State<MachineState>,
    Path(machine_id): Path<String>,
) -> Result<Json<Vec<Environment>>, ApiError> {
    Ok(Json(state.service.list_environments(&machine_id).await?))
}

async fn create_machine_environment(
    State(state): State<MachineState>,
    Path(machine_id): Path<String>,
    payload: Result<Json<CreateMachineEnvironmentRequest>, JsonRejection>,
) -> Result<Response, ApiError> {
    let Json(request) = payload.map_err(|error| ApiError::invalid_request(error.body_text()))?;
    let environment = state
        .service
        .create_environment(&machine_id, &request)
        .await?;
    Ok((StatusCode::CREATED, Json(environment)).into_response())
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

async fn list_sandbox_machines(
    State(state): State<MachineState>,
    Path(account_id): Path<Uuid>,
) -> Result<Json<Vec<SandboxMachine>>, ApiError> {
    Ok(Json(state.service.list_sandbox_machines(account_id).await?))
}

async fn create_sandbox(
    State(state): State<MachineState>,
    Path(account_id): Path<Uuid>,
    payload: Result<Json<CreateSandboxRequest>, JsonRejection>,
) -> Result<Response, ApiError> {
    let Json(request) = payload.map_err(|error| ApiError::invalid_request(error.body_text()))?;
    let sandbox = state.service.create_sandbox(account_id, &request).await?;
    Ok((StatusCode::CREATED, Json::<SandboxCreated>(sandbox)).into_response())
}

async fn create_sandbox_snapshot(
    State(state): State<MachineState>,
    Path((account_id, sandbox_id)): Path<(Uuid, String)>,
    payload: Result<Json<CreateSandboxSnapshotRequest>, JsonRejection>,
) -> Result<Response, ApiError> {
    let Json(request) = payload.map_err(|error| ApiError::invalid_request(error.body_text()))?;
    let snapshot = state
        .service
        .create_sandbox_snapshot(account_id, &sandbox_id, &request)
        .await?;
    Ok((StatusCode::CREATED, Json(snapshot)).into_response())
}

async fn list_snapshots(
    State(state): State<MachineState>,
    Query(query): Query<SnapshotQuery>,
) -> Result<Json<Vec<Snapshot>>, ApiError> {
    Ok(Json(state.service.list_snapshots(&query).await?))
}

async fn get_snapshot(
    State(state): State<MachineState>,
    Path(snapshot_id): Path<Uuid>,
) -> Result<Json<Snapshot>, ApiError> {
    Ok(Json(state.service.get_snapshot(snapshot_id).await?))
}

async fn update_snapshot_name(
    State(state): State<MachineState>,
    Path(snapshot_id): Path<Uuid>,
    payload: Result<Json<UpdateNameRequest>, JsonRejection>,
) -> Result<Json<Snapshot>, ApiError> {
    let Json(request) = payload.map_err(|error| ApiError::invalid_request(error.body_text()))?;
    Ok(Json(
        state
            .service
            .update_snapshot_name(snapshot_id, &request)
            .await?,
    ))
}

async fn create_snapshot(
    State(state): State<MachineState>,
    payload: Result<Json<CreateSnapshotRequest>, JsonRejection>,
) -> Result<Response, ApiError> {
    let Json(request) = payload.map_err(|error| ApiError::invalid_request(error.body_text()))?;
    let snapshot = state.service.create_snapshot(&request).await?;
    Ok((StatusCode::CREATED, Json(snapshot)).into_response())
}

async fn delete_snapshot(
    State(state): State<MachineState>,
    Path(snapshot_id): Path<Uuid>,
) -> Result<StatusCode, ApiError> {
    state.service.delete_snapshot(snapshot_id).await?;
    Ok(StatusCode::NO_CONTENT)
}

async fn list_sandbox_environment_templates(
    State(state): State<MachineState>,
) -> Result<Json<Vec<SandboxEnvironmentTemplate>>, ApiError> {
    Ok(Json(
        state.service.list_sandbox_environment_templates().await?,
    ))
}

async fn get_sandbox_environment_template(
    State(state): State<MachineState>,
    Path(template_id): Path<Uuid>,
) -> Result<Json<SandboxEnvironmentTemplate>, ApiError> {
    Ok(Json(
        state
            .service
            .get_sandbox_environment_template(template_id)
            .await?,
    ))
}

async fn create_sandbox_environment_template(
    State(state): State<MachineState>,
    payload: Result<Json<CreateSandboxEnvironmentTemplateRequest>, JsonRejection>,
) -> Result<Response, ApiError> {
    let Json(request) = payload.map_err(|error| ApiError::invalid_request(error.body_text()))?;
    let template = state
        .service
        .create_sandbox_environment_template(&request)
        .await?;
    Ok((StatusCode::CREATED, Json(template)).into_response())
}

async fn update_sandbox_environment_template(
    State(state): State<MachineState>,
    Path(template_id): Path<Uuid>,
    payload: Result<Json<UpdateSandboxEnvironmentTemplateRequest>, JsonRejection>,
) -> Result<Json<SandboxEnvironmentTemplate>, ApiError> {
    let Json(request) = payload.map_err(|error| ApiError::invalid_request(error.body_text()))?;
    Ok(Json(
        state
            .service
            .update_sandbox_environment_template(template_id, &request)
            .await?,
    ))
}

async fn delete_sandbox_environment_template(
    State(state): State<MachineState>,
    Path(template_id): Path<Uuid>,
) -> Result<StatusCode, ApiError> {
    state
        .service
        .delete_sandbox_environment_template(template_id)
        .await?;
    Ok(StatusCode::NO_CONTENT)
}

async fn materialize_sandbox_environment_template(
    State(state): State<MachineState>,
    Path(template_id): Path<Uuid>,
) -> Result<Response, ApiError> {
    let instance = state
        .service
        .materialize_sandbox_environment_template(template_id)
        .await?;
    Ok((StatusCode::CREATED, Json(instance)).into_response())
}

async fn list_sandbox_environment_instances(
    State(state): State<MachineState>,
    Path(template_id): Path<Uuid>,
) -> Result<Json<Vec<SandboxEnvironmentInstance>>, ApiError> {
    Ok(Json(
        state
            .service
            .list_sandbox_environment_instances(template_id)
            .await?,
    ))
}

async fn delete_environment(
    State(state): State<MachineState>,
    Path(environment_id): Path<String>,
) -> Result<StatusCode, ApiError> {
    state.service.delete_environment(&environment_id).await?;
    Ok(StatusCode::NO_CONTENT)
}

async fn update_environment_name(
    State(state): State<MachineState>,
    Path(environment_id): Path<String>,
    payload: Result<Json<UpdateNameRequest>, JsonRejection>,
) -> Result<Json<Environment>, ApiError> {
    let Json(request) = payload.map_err(|error| ApiError::invalid_request(error.body_text()))?;
    Ok(Json(
        state
            .service
            .update_environment_name(&environment_id, &request)
            .await?,
    ))
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
