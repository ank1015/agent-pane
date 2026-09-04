use axum::{
    Json, Router,
    extract::{
        DefaultBodyLimit, Path, State,
        rejection::{JsonRejection, PathRejection},
    },
    http::{HeaderValue, StatusCode, header::CACHE_CONTROL},
    middleware,
    response::{IntoResponse, Response},
    routing::{get, patch},
};

use super::{CreateE2bAccountInput, MachineService, UpdateMachineInput};
use crate::error::ApiError;

pub fn router(service: MachineService) -> Router {
    Router::new()
        .route("/api/machines", get(list_machines))
        .route(
            "/api/machines/{machine_id}",
            patch(update_machine).delete(delete_machine),
        )
        .route(
            "/api/machines/e2b-accounts",
            get(list_accounts).post(create_account),
        )
        .layer(DefaultBodyLimit::max(16 * 1024))
        .layer(middleware::map_response(no_store))
        .with_state(service)
}

async fn update_machine(
    State(service): State<MachineService>,
    path: Result<Path<uuid::Uuid>, PathRejection>,
    payload: Result<Json<UpdateMachineInput>, JsonRejection>,
) -> Result<Response, ApiError> {
    let Path(id) = path.map_err(|_| ApiError::InvalidRequest("Machine ID must be a UUID."))?;
    let Json(request) = payload.map_err(|error| ApiError::InvalidJson(error.status()))?;
    Ok(Json(service.update_machine(id, request).await?).into_response())
}

async fn delete_machine(
    State(service): State<MachineService>,
    path: Result<Path<uuid::Uuid>, PathRejection>,
) -> Result<Response, ApiError> {
    let Path(id) = path.map_err(|_| ApiError::InvalidRequest("Machine ID must be a UUID."))?;
    service.delete_machine(id).await?;
    Ok(StatusCode::NO_CONTENT.into_response())
}

async fn list_accounts(State(service): State<MachineService>) -> Result<Response, ApiError> {
    Ok(Json(service.list_e2b_accounts().await?).into_response())
}

async fn create_account(
    State(service): State<MachineService>,
    payload: Result<Json<CreateE2bAccountInput>, JsonRejection>,
) -> Result<Response, ApiError> {
    let Json(request) = payload.map_err(|error| ApiError::InvalidJson(error.status()))?;
    let account = service.create_e2b_account(request).await?;
    Ok((StatusCode::CREATED, Json(account)).into_response())
}

async fn no_store(mut response: Response) -> Response {
    response
        .headers_mut()
        .insert(CACHE_CONTROL, HeaderValue::from_static("no-store"));
    response
}

async fn list_machines(State(service): State<MachineService>) -> Result<Response, ApiError> {
    Ok(Json(service.list_machines().await?).into_response())
}
