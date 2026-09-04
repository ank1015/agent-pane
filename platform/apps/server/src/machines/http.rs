use axum::{
    Json, Router,
    extract::{DefaultBodyLimit, State, rejection::JsonRejection},
    http::{HeaderValue, StatusCode, header::CACHE_CONTROL},
    middleware,
    response::{IntoResponse, Response},
    routing::get,
};

use super::{CreateE2bAccountInput, MachineService};
use crate::error::ApiError;

pub fn router(service: MachineService) -> Router {
    Router::new()
        .route("/api/machines", get(list_machines))
        .route(
            "/api/machines/e2b-accounts",
            get(list_accounts).post(create_account),
        )
        .layer(DefaultBodyLimit::max(16 * 1024))
        .layer(middleware::map_response(no_store))
        .with_state(service)
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
