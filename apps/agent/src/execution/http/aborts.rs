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
    execution::{AbortListQuery, RequestRunAbort, ResumeRun, aborting},
};

pub(super) fn router() -> Router<AppState> {
    Router::new()
        .route("/v1/runs/{run_id}/abort", post(abort_run))
        .route("/v1/runs/{run_id}/resume", post(resume_run))
        .route("/v1/runs/{run_id}/aborts", get(list_aborts))
        .route("/v1/aborts/{abort_id}", get(get_abort))
}

async fn abort_run(
    State(state): State<AppState>,
    path: Result<Path<String>, PathRejection>,
    payload: Result<Json<RequestRunAbort>, JsonRejection>,
) -> Result<Response, ApiError> {
    let outcome = aborting::request(
        state.database(),
        state.execution_policy(),
        uuid_path("run_id", path)?,
        json_body(payload)?,
    )
    .await?;
    let status = if outcome.created {
        StatusCode::ACCEPTED
    } else {
        StatusCode::OK
    };
    Ok(json_response(status, outcome.value))
}

async fn resume_run(
    State(state): State<AppState>,
    path: Result<Path<String>, PathRejection>,
    payload: Result<Json<ResumeRun>, JsonRejection>,
) -> Result<Response, ApiError> {
    let outcome = aborting::resume(
        state.database(),
        uuid_path("run_id", path)?,
        json_body(payload)?,
    )
    .await?;
    let status = if outcome.created {
        StatusCode::ACCEPTED
    } else {
        StatusCode::OK
    };
    Ok(json_response(status, outcome.value))
}

async fn list_aborts(
    State(state): State<AppState>,
    path: Result<Path<String>, PathRejection>,
    query: Result<Query<AbortListQuery>, QueryRejection>,
) -> Result<Response, ApiError> {
    let Query(query) = query.map_err(|error| ApiError::invalid_request(error.body_text()))?;
    let page = aborting::list(state.database(), uuid_path("run_id", path)?, query).await?;
    Ok(json_response(StatusCode::OK, page))
}

async fn get_abort(
    State(state): State<AppState>,
    path: Result<Path<String>, PathRejection>,
) -> Result<Response, ApiError> {
    let abort = aborting::get(state.database(), uuid_path("abort_id", path)?).await?;
    Ok(json_response(StatusCode::OK, abort))
}
