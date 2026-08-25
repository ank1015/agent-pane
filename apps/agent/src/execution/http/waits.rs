use axum::{
    Json, Router,
    extract::{
        Path, Query, State,
        rejection::{JsonRejection, PathRejection, QueryRejection},
    },
    http::StatusCode,
    response::Response,
    routing::{get, post},
};

use super::runs::{json_body, json_response, uuid_path};
use crate::{
    api_error::ApiError,
    app::AppState,
    execution::{ResolveRunWait, WaitListQuery, waiting},
};

pub(super) fn router() -> Router<AppState> {
    Router::new()
        .route("/v1/waits", get(list_waits))
        .route("/v1/waits/{wait_id}", get(get_wait))
        .route("/v1/waits/{wait_id}/resolve", post(resolve_wait))
        .route("/v1/runs/{run_id}/waits", get(list_run_waits))
}

async fn list_waits(
    State(state): State<AppState>,
    query: Result<Query<WaitListQuery>, QueryRejection>,
) -> Result<Response, ApiError> {
    list(state, None, query).await
}

async fn list_run_waits(
    State(state): State<AppState>,
    path: Result<Path<String>, PathRejection>,
    query: Result<Query<WaitListQuery>, QueryRejection>,
) -> Result<Response, ApiError> {
    list(state, Some(uuid_path("run_id", path)?), query).await
}

async fn list(
    state: AppState,
    run_id: Option<uuid::Uuid>,
    query: Result<Query<WaitListQuery>, QueryRejection>,
) -> Result<Response, ApiError> {
    let Query(query) = query.map_err(|error| ApiError::invalid_request(error.body_text()))?;
    let page = waiting::list(state.database(), run_id, query).await?;
    Ok(json_response(StatusCode::OK, page))
}

async fn get_wait(
    State(state): State<AppState>,
    path: Result<Path<String>, PathRejection>,
) -> Result<Response, ApiError> {
    let wait = waiting::get(state.database(), uuid_path("wait_id", path)?).await?;
    Ok(json_response(StatusCode::OK, wait))
}

async fn resolve_wait(
    State(state): State<AppState>,
    path: Result<Path<String>, PathRejection>,
    payload: Result<Json<ResolveRunWait>, JsonRejection>,
) -> Result<Response, ApiError> {
    let outcome = waiting::resolve(
        state.database(),
        uuid_path("wait_id", path)?,
        json_body(payload)?,
    )
    .await?;
    Ok(json_response(StatusCode::OK, outcome.value))
}
