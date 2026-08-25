use axum::{
    Json, Router,
    extract::{
        Path, Query, State,
        rejection::{JsonRejection, PathRejection, QueryRejection},
    },
    http::{HeaderMap, HeaderValue, StatusCode, header::RETRY_AFTER},
    response::{IntoResponse as _, Response},
    routing::{get, post},
};
use serde::Deserialize;

use super::runs::{json_body, json_response, uuid_path};
use crate::{
    api_error::ApiError,
    app::AppState,
    execution::{
        AcknowledgeRunAbort, AppendRunMessages, ClaimRun, CompleteRunTurn, FailRunTurn,
        HeartbeatRun, RequestRunWait, aborting, finishing,
        leasing::{self, LeaseToken},
        messages, waiting,
    },
};

const LEASE_TOKEN_HEADER: &str = "x-agent-lease-token";

pub(super) fn router() -> Router<AppState> {
    Router::new()
        .route("/v1/worker/runs/claim", post(claim_run))
        .route("/v1/worker/runs/{run_id}/heartbeat", post(heartbeat_run))
        .route(
            "/v1/worker/runs/{run_id}/messages",
            get(list_messages).post(append_messages),
        )
        .route("/v1/worker/runs/{run_id}/complete", post(complete_run))
        .route("/v1/worker/runs/{run_id}/fail", post(fail_run))
        .route("/v1/worker/runs/{run_id}/wait", post(request_wait))
        .route(
            "/v1/worker/runs/{run_id}/abort/acknowledge",
            post(acknowledge_abort),
        )
}

async fn claim_run(
    State(state): State<AppState>,
    headers: HeaderMap,
    payload: Result<Json<ClaimRun>, JsonRejection>,
) -> Result<Response, ApiError> {
    let token = lease_token(&headers)?;
    let outcome = leasing::claim(
        state.database(),
        state.execution_policy(),
        json_body(payload)?,
        &token,
    )
    .await?;
    match outcome {
        Some(claimed) => Ok(json_response(StatusCode::OK, claimed)),
        None => Ok(no_claim_response()),
    }
}

async fn heartbeat_run(
    State(state): State<AppState>,
    headers: HeaderMap,
    path: Result<Path<String>, PathRejection>,
    payload: Result<Json<HeartbeatRun>, JsonRejection>,
) -> Result<Response, ApiError> {
    let token = lease_token(&headers)?;
    let heartbeat = leasing::heartbeat(
        state.database(),
        state.execution_policy(),
        uuid_path("run_id", path)?,
        json_body(payload)?,
        &token,
    )
    .await?;
    Ok(json_response(StatusCode::OK, heartbeat))
}

async fn list_messages(
    State(state): State<AppState>,
    headers: HeaderMap,
    path: Result<Path<String>, PathRejection>,
    query: Result<Query<LeasedMessagesQuery>, QueryRejection>,
) -> Result<Response, ApiError> {
    let token = lease_token(&headers)?;
    let Query(query) = query.map_err(|error| ApiError::invalid_request(error.body_text()))?;
    let page = messages::list(
        state.database(),
        uuid_path("run_id", path)?,
        query.lease_version,
        &token,
        query.after_revision,
        query.limit,
    )
    .await?;
    Ok(json_response(StatusCode::OK, page))
}

async fn append_messages(
    State(state): State<AppState>,
    headers: HeaderMap,
    path: Result<Path<String>, PathRejection>,
    payload: Result<Json<AppendRunMessages>, JsonRejection>,
) -> Result<Response, ApiError> {
    let token = lease_token(&headers)?;
    let appended = messages::append(
        state.database(),
        state.execution_policy(),
        uuid_path("run_id", path)?,
        json_body(payload)?,
        &token,
    )
    .await?;
    Ok(json_response(StatusCode::CREATED, appended))
}

async fn complete_run(
    State(state): State<AppState>,
    headers: HeaderMap,
    path: Result<Path<String>, PathRejection>,
    payload: Result<Json<CompleteRunTurn>, JsonRejection>,
) -> Result<Response, ApiError> {
    let token = lease_token(&headers)?;
    let completed = finishing::complete(
        state.database(),
        uuid_path("run_id", path)?,
        json_body(payload)?,
        &token,
    )
    .await?;
    Ok(json_response(StatusCode::OK, completed))
}

async fn fail_run(
    State(state): State<AppState>,
    headers: HeaderMap,
    path: Result<Path<String>, PathRejection>,
    payload: Result<Json<FailRunTurn>, JsonRejection>,
) -> Result<Response, ApiError> {
    let token = lease_token(&headers)?;
    let failed = finishing::fail(
        state.database(),
        uuid_path("run_id", path)?,
        json_body(payload)?,
        &token,
    )
    .await?;
    Ok(json_response(StatusCode::OK, failed))
}

async fn request_wait(
    State(state): State<AppState>,
    headers: HeaderMap,
    path: Result<Path<String>, PathRejection>,
    payload: Result<Json<RequestRunWait>, JsonRejection>,
) -> Result<Response, ApiError> {
    let token = lease_token(&headers)?;
    let outcome = waiting::request(
        state.database(),
        uuid_path("run_id", path)?,
        json_body(payload)?,
        &token,
    )
    .await?;
    let status = if outcome.created {
        StatusCode::ACCEPTED
    } else {
        StatusCode::OK
    };
    Ok(json_response(status, outcome.value))
}

async fn acknowledge_abort(
    State(state): State<AppState>,
    headers: HeaderMap,
    path: Result<Path<String>, PathRejection>,
    payload: Result<Json<AcknowledgeRunAbort>, JsonRejection>,
) -> Result<Response, ApiError> {
    let token = lease_token(&headers)?;
    let outcome = aborting::acknowledge(
        state.database(),
        uuid_path("run_id", path)?,
        json_body(payload)?,
        &token,
    )
    .await?;
    Ok(json_response(StatusCode::OK, outcome.value))
}

fn lease_token(headers: &HeaderMap) -> Result<LeaseToken, ApiError> {
    let value = headers.get(LEASE_TOKEN_HEADER).ok_or_else(|| {
        ApiError::new(
            StatusCode::UNAUTHORIZED,
            "missing_run_lease",
            "X-Agent-Lease-Token is required",
        )
    })?;
    let value = value.to_str().map_err(|_| {
        ApiError::new(
            StatusCode::UNAUTHORIZED,
            "invalid_run_lease",
            "X-Agent-Lease-Token is invalid",
        )
    })?;
    LeaseToken::parse(value).map_err(Into::into)
}

fn no_claim_response() -> Response {
    let mut response = StatusCode::NO_CONTENT.into_response();
    response
        .headers_mut()
        .insert(RETRY_AFTER, HeaderValue::from_static("1"));
    response.headers_mut().insert(
        axum::http::header::CACHE_CONTROL,
        HeaderValue::from_static("no-store"),
    );
    response
}

#[derive(Clone, Copy, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct LeasedMessagesQuery {
    lease_version: u64,
    after_revision: Option<u64>,
    limit: Option<u32>,
}
