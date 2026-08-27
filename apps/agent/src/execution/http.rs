use axum::{
    Json, Router,
    extract::{
        Path, Query, State,
        rejection::{JsonRejection, PathRejection, QueryRejection},
    },
    http::{HeaderValue, StatusCode, header::CACHE_CONTROL},
    response::{IntoResponse, Response},
    routing::{get, post},
};
use serde::Serialize;
use serde_json::json;
use uuid::Uuid;

use super::{
    AbortListQuery, AppendSessionMessages, ExecutionError, HarnessMessageListQuery,
    QueueRunMessage, QueuedRunMessageListQuery, RequestRunAbort, ResolveRunWait, StartRun,
    WaitListQuery, aborts, messages, runs, waits,
};
use crate::{api_error::ApiError, app::AppState};

pub fn router() -> Router<AppState> {
    Router::new()
        .route("/v1/sessions/{session_id}/runs", post(start_run))
        .route("/v1/runs/{run_id}", get(get_run))
        .route(
            "/v1/runs/{run_id}/messages",
            get(list_queued_messages).post(queue_message),
        )
        .route(
            "/v1/runs/{run_id}/messages/{message_id}",
            get(get_queued_message),
        )
        .route("/v1/runs/{run_id}/waits", get(list_waits))
        .route("/v1/runs/{run_id}/waits/{wait_id}", get(get_wait))
        .route(
            "/v1/runs/{run_id}/waits/{wait_id}/resolve",
            post(resolve_wait),
        )
        .route("/v1/runs/{run_id}/abort", post(abort_run))
        .route("/v1/runs/{run_id}/aborts", get(list_aborts))
        .route("/v1/runs/{run_id}/aborts/{abort_id}", get(get_abort))
}

pub fn harness_router() -> Router<AppState> {
    Router::new().route(
        "/v1/harness/runs/{run_id}/messages",
        get(list_harness_messages).post(append_harness_messages),
    )
}

async fn start_run(
    State(state): State<AppState>,
    path: Result<Path<String>, PathRejection>,
    payload: Result<Json<StartRun>, JsonRejection>,
) -> Result<Response, ApiError> {
    let outcome = runs::start(
        state.database(),
        state.execution_policy(),
        uuid_path("session_id", path)?,
        json_body(payload)?,
    )
    .await?;
    Ok(json_response(
        if outcome.created {
            StatusCode::ACCEPTED
        } else {
            StatusCode::OK
        },
        outcome.value,
    ))
}
async fn get_run(
    State(state): State<AppState>,
    path: Result<Path<String>, PathRejection>,
) -> Result<Response, ApiError> {
    Ok(json_response(
        StatusCode::OK,
        runs::get(state.database(), uuid_path("run_id", path)?).await?,
    ))
}
async fn queue_message(
    State(state): State<AppState>,
    path: Result<Path<String>, PathRejection>,
    payload: Result<Json<QueueRunMessage>, JsonRejection>,
) -> Result<Response, ApiError> {
    let outcome = messages::queue(
        state.database(),
        uuid_path("run_id", path)?,
        json_body(payload)?,
    )
    .await?;
    Ok(json_response(
        if outcome.created {
            StatusCode::ACCEPTED
        } else {
            StatusCode::OK
        },
        outcome.value,
    ))
}
async fn list_queued_messages(
    State(state): State<AppState>,
    path: Result<Path<String>, PathRejection>,
    query: Result<Query<QueuedRunMessageListQuery>, QueryRejection>,
) -> Result<Response, ApiError> {
    Ok(json_response(
        StatusCode::OK,
        messages::list_queued(
            state.database(),
            uuid_path("run_id", path)?,
            query_body(query)?,
        )
        .await?,
    ))
}
async fn get_queued_message(
    State(state): State<AppState>,
    path: Result<Path<(String, String)>, PathRejection>,
) -> Result<Response, ApiError> {
    let (run_id, message_id) = two_uuid_path(path, "run_id", "message_id")?;
    Ok(json_response(
        StatusCode::OK,
        messages::get_queued(state.database(), run_id, message_id).await?,
    ))
}
async fn list_waits(
    State(state): State<AppState>,
    path: Result<Path<String>, PathRejection>,
    query: Result<Query<WaitListQuery>, QueryRejection>,
) -> Result<Response, ApiError> {
    Ok(json_response(
        StatusCode::OK,
        waits::list(
            state.database(),
            uuid_path("run_id", path)?,
            query_body(query)?,
        )
        .await?,
    ))
}
async fn get_wait(
    State(state): State<AppState>,
    path: Result<Path<(String, String)>, PathRejection>,
) -> Result<Response, ApiError> {
    let (run_id, wait_id) = two_uuid_path(path, "run_id", "wait_id")?;
    Ok(json_response(
        StatusCode::OK,
        waits::get(state.database(), run_id, wait_id).await?,
    ))
}
async fn resolve_wait(
    State(state): State<AppState>,
    path: Result<Path<(String, String)>, PathRejection>,
    payload: Result<Json<ResolveRunWait>, JsonRejection>,
) -> Result<Response, ApiError> {
    let (run_id, wait_id) = two_uuid_path(path, "run_id", "wait_id")?;
    Ok(json_response(
        StatusCode::OK,
        waits::resolve(state.database(), run_id, wait_id, json_body(payload)?).await?,
    ))
}
async fn abort_run(
    State(state): State<AppState>,
    path: Result<Path<String>, PathRejection>,
    payload: Result<Json<RequestRunAbort>, JsonRejection>,
) -> Result<Response, ApiError> {
    let outcome = aborts::request(
        state.database(),
        uuid_path("run_id", path)?,
        json_body(payload)?,
    )
    .await?;
    Ok(json_response(
        if outcome.created {
            StatusCode::ACCEPTED
        } else {
            StatusCode::OK
        },
        outcome.value,
    ))
}
async fn list_aborts(
    State(state): State<AppState>,
    path: Result<Path<String>, PathRejection>,
    query: Result<Query<AbortListQuery>, QueryRejection>,
) -> Result<Response, ApiError> {
    Ok(json_response(
        StatusCode::OK,
        aborts::list(
            state.database(),
            uuid_path("run_id", path)?,
            query_body(query)?,
        )
        .await?,
    ))
}
async fn get_abort(
    State(state): State<AppState>,
    path: Result<Path<(String, String)>, PathRejection>,
) -> Result<Response, ApiError> {
    let (run_id, abort_id) = two_uuid_path(path, "run_id", "abort_id")?;
    Ok(json_response(
        StatusCode::OK,
        aborts::get(state.database(), run_id, abort_id).await?,
    ))
}
async fn list_harness_messages(
    State(state): State<AppState>,
    path: Result<Path<String>, PathRejection>,
    query: Result<Query<HarnessMessageListQuery>, QueryRejection>,
) -> Result<Response, ApiError> {
    Ok(json_response(
        StatusCode::OK,
        messages::list_harness(
            state.database(),
            uuid_path("run_id", path)?,
            query_body(query)?,
        )
        .await?,
    ))
}
async fn append_harness_messages(
    State(state): State<AppState>,
    path: Result<Path<String>, PathRejection>,
    payload: Result<Json<AppendSessionMessages>, JsonRejection>,
) -> Result<Response, ApiError> {
    Ok(json_response(
        StatusCode::OK,
        messages::append_harness(
            state.database(),
            state.execution_policy(),
            uuid_path("run_id", path)?,
            json_body(payload)?,
        )
        .await?,
    ))
}

fn json_body<T>(payload: Result<Json<T>, JsonRejection>) -> Result<T, ApiError> {
    payload
        .map(|Json(v)| v)
        .map_err(|e| ApiError::invalid_request(e.body_text()))
}
fn query_body<T>(query: Result<Query<T>, QueryRejection>) -> Result<T, ApiError> {
    query
        .map(|Query(v)| v)
        .map_err(|e| ApiError::invalid_request(e.body_text()))
}
fn uuid_path(
    name: &'static str,
    path: Result<Path<String>, PathRejection>,
) -> Result<Uuid, ApiError> {
    let Path(value) = path.map_err(|e| ApiError::invalid_request(e.body_text()))?;
    Uuid::parse_str(&value).map_err(|_| ApiError::invalid_request(format!("{name} must be a UUID")))
}
fn two_uuid_path(
    path: Result<Path<(String, String)>, PathRejection>,
    first: &'static str,
    second: &'static str,
) -> Result<(Uuid, Uuid), ApiError> {
    let Path((a, b)) = path.map_err(|e| ApiError::invalid_request(e.body_text()))?;
    Ok((
        Uuid::parse_str(&a)
            .map_err(|_| ApiError::invalid_request(format!("{first} must be a UUID")))?,
        Uuid::parse_str(&b)
            .map_err(|_| ApiError::invalid_request(format!("{second} must be a UUID")))?,
    ))
}
fn json_response<T: Serialize>(status: StatusCode, body: T) -> Response {
    let mut response = (status, Json(body)).into_response();
    response
        .headers_mut()
        .insert(CACHE_CONTROL, HeaderValue::from_static("no-store"));
    response
}

impl From<ExecutionError> for ApiError {
    fn from(error: ExecutionError) -> Self {
        match error {
            ExecutionError::Validation(ref validation) => {
                ApiError::invalid_request(validation.to_string())
                    .with_details(json!({"issues": validation.issues}))
            }
            ExecutionError::SessionNotFound(_) => not_found("session_not_found", error),
            ExecutionError::RunNotFound(_) => not_found("run_not_found", error),
            ExecutionError::QueuedMessageNotFound(_) => {
                not_found("queued_run_message_not_found", error)
            }
            ExecutionError::WaitNotFound(_) => not_found("wait_not_found", error),
            ExecutionError::AbortNotFound(_) => not_found("abort_not_found", error),
            ExecutionError::InvalidHarnessConfiguration(_) => ApiError::new(
                StatusCode::UNPROCESSABLE_ENTITY,
                "invalid_harness_configuration",
                error.to_string(),
            ),
            ExecutionError::RunLimitExceeded { .. } => ApiError::new(
                StatusCode::UNPROCESSABLE_ENTITY,
                "run_limit_exceeded",
                error.to_string(),
            ),
            ExecutionError::MessageBatchTooLarge(_) => ApiError::new(
                StatusCode::UNPROCESSABLE_ENTITY,
                "message_batch_too_large",
                error.to_string(),
            ),
            ExecutionError::InvalidMessagePageSize
            | ExecutionError::InvalidQueuedMessagePageSize
            | ExecutionError::InvalidQueuedMessageAfterSequence
            | ExecutionError::InvalidWaitPageSize
            | ExecutionError::InvalidWaitCursor
            | ExecutionError::InvalidAbortPageSize
            | ExecutionError::InvalidAbortCursor => ApiError::invalid_request(error.to_string()),
            ExecutionError::InvalidStoredData(_) | ExecutionError::Database(_) => {
                tracing::error!(%error, "execution request failed");
                ApiError::internal()
            }
            _ => conflict(error_code(&error), error),
        }
    }
}
fn error_code(error: &ExecutionError) -> &'static str {
    match error {
        ExecutionError::RunIdConflict(_) => "run_id_conflict",
        ExecutionError::MessageIdConflict(_) => "session_message_id_conflict",
        ExecutionError::SessionRevisionConflict { .. } => "session_revision_conflict",
        ExecutionError::RunStateConflict { .. } => "run_state_conflict",
        ExecutionError::RunTurnConflict { .. } => "run_turn_conflict",
        ExecutionError::SessionHasActiveRun(_) => "session_has_active_run",
        ExecutionError::HarnessNotAvailable(_) => "harness_not_available",
        ExecutionError::HarnessRevisionNotAvailable(_) => "harness_revision_not_available",
        ExecutionError::RunNotActive { .. } => "run_not_active",
        ExecutionError::RunTurnLimitReached { .. } => "run_turn_limit_reached",
        ExecutionError::InvalidFinalMessage => "invalid_final_message",
        ExecutionError::WaitIdConflict(_) => "wait_id_conflict",
        ExecutionError::HarnessWaitIdConflict(_) => "harness_wait_id_conflict",
        ExecutionError::RunAlreadyWaiting(_) => "run_already_waiting",
        ExecutionError::WaitNotPending(_) => "wait_not_pending",
        ExecutionError::AbortIdConflict(_) => "abort_id_conflict",
        ExecutionError::RunNotAbortable { .. } => "run_not_abortable",
        ExecutionError::CommandIdConflict(_) => "command_id_conflict",
        _ => "conflict",
    }
}
fn conflict(code: &'static str, error: ExecutionError) -> ApiError {
    ApiError::new(StatusCode::CONFLICT, code, error.to_string())
}
fn not_found(code: &'static str, error: ExecutionError) -> ApiError {
    ApiError::new(StatusCode::NOT_FOUND, code, error.to_string())
}
