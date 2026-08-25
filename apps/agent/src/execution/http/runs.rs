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

use super::super::{
    ExecutionError, QueueRunMessage, QueuedRunMessageListQuery, StartRun, start, steer,
};
use crate::{api_error::ApiError, app::AppState};

pub(super) fn router() -> Router<AppState> {
    Router::new()
        .route("/v1/sessions/{session_id}/runs", post(start_run))
        .route("/v1/runs/{run_id}", get(get_run))
        .route(
            "/v1/runs/{run_id}/messages",
            get(list_queued_messages).post(queue_message),
        )
        .route(
            "/v1/runs/{run_id}/messages/{session_message_id}",
            get(get_queued_message),
        )
}

async fn start_run(
    State(state): State<AppState>,
    path: Result<Path<String>, PathRejection>,
    payload: Result<Json<StartRun>, JsonRejection>,
) -> Result<Response, ApiError> {
    let outcome = start::start(
        state.database(),
        state.execution_policy(),
        uuid_path("session_id", path)?,
        json_body(payload)?,
    )
    .await?;
    let status = if outcome.created {
        StatusCode::ACCEPTED
    } else {
        StatusCode::OK
    };
    Ok(json_response(status, outcome.accepted))
}

async fn get_run(
    State(state): State<AppState>,
    path: Result<Path<String>, PathRejection>,
) -> Result<Response, ApiError> {
    let run = start::get(state.database(), uuid_path("run_id", path)?).await?;
    Ok(json_response(StatusCode::OK, run))
}

async fn queue_message(
    State(state): State<AppState>,
    path: Result<Path<String>, PathRejection>,
    payload: Result<Json<QueueRunMessage>, JsonRejection>,
) -> Result<Response, ApiError> {
    let outcome = steer::queue(
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
    Ok(json_response(status, outcome.message))
}

async fn list_queued_messages(
    State(state): State<AppState>,
    path: Result<Path<String>, PathRejection>,
    query: Result<Query<QueuedRunMessageListQuery>, QueryRejection>,
) -> Result<Response, ApiError> {
    let query = query
        .map(|Query(query)| query)
        .map_err(|error| ApiError::invalid_request(error.body_text()))?;
    let page = steer::list(state.database(), uuid_path("run_id", path)?, query).await?;
    Ok(json_response(StatusCode::OK, page))
}

async fn get_queued_message(
    State(state): State<AppState>,
    path: Result<Path<(String, String)>, PathRejection>,
) -> Result<Response, ApiError> {
    let Path((run_id, session_message_id)) =
        path.map_err(|error| ApiError::invalid_request(error.body_text()))?;
    let run_id =
        Uuid::parse_str(&run_id).map_err(|_| ApiError::invalid_request("run_id must be a UUID"))?;
    let session_message_id = Uuid::parse_str(&session_message_id)
        .map_err(|_| ApiError::invalid_request("session_message_id must be a UUID"))?;
    let message = steer::get(state.database(), run_id, session_message_id).await?;
    Ok(json_response(StatusCode::OK, message))
}

pub(super) fn json_body<T>(payload: Result<Json<T>, JsonRejection>) -> Result<T, ApiError> {
    payload
        .map(|Json(value)| value)
        .map_err(|error| ApiError::invalid_request(error.body_text()))
}

pub(super) fn uuid_path(
    name: &'static str,
    path: Result<Path<String>, PathRejection>,
) -> Result<Uuid, ApiError> {
    let Path(value) = path.map_err(|error| ApiError::invalid_request(error.body_text()))?;
    Uuid::parse_str(&value).map_err(|_| ApiError::invalid_request(format!("{name} must be a UUID")))
}

pub(super) fn json_response<T: Serialize>(status: StatusCode, body: T) -> Response {
    let mut response = (status, Json(body)).into_response();
    response
        .headers_mut()
        .insert(CACHE_CONTROL, HeaderValue::from_static("no-store"));
    response
}

impl From<ExecutionError> for ApiError {
    fn from(error: ExecutionError) -> Self {
        match error {
            ExecutionError::Validation(error) => ApiError::invalid_request(error.to_string())
                .with_details(json!({ "issues": error.issues })),
            ExecutionError::SessionNotFound(_) => ApiError::new(
                StatusCode::NOT_FOUND,
                "session_not_found",
                error.to_string(),
            ),
            ExecutionError::RunNotFound(_) => {
                ApiError::new(StatusCode::NOT_FOUND, "run_not_found", error.to_string())
            }
            ExecutionError::QueuedMessageNotFound(_) => ApiError::new(
                StatusCode::NOT_FOUND,
                "queued_run_message_not_found",
                error.to_string(),
            ),
            ExecutionError::RunIdConflict(_) => conflict("run_id_conflict", error),
            ExecutionError::MessageIdConflict(_) => conflict("session_message_id_conflict", error),
            ExecutionError::SessionRevisionConflict { .. } => {
                conflict("session_revision_conflict", error)
            }
            ExecutionError::SessionHasActiveRun(_) => conflict("session_has_active_run", error),
            ExecutionError::HarnessNotAvailable(_) => conflict("harness_not_available", error),
            ExecutionError::HarnessRevisionNotAvailable(_) => {
                conflict("harness_revision_not_available", error)
            }
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
            ExecutionError::LeaseIdConflict(_) => conflict("lease_id_conflict", error),
            ExecutionError::LeaseLost => conflict("run_lease_lost", error),
            ExecutionError::InvalidLeaseToken => ApiError::new(
                StatusCode::UNAUTHORIZED,
                "invalid_run_lease",
                error.to_string(),
            ),
            ExecutionError::RunStateConflict { .. } => conflict("run_state_conflict", error),
            ExecutionError::RunNotRunnable { .. } => conflict("run_not_runnable", error),
            ExecutionError::RunTurnLimitReached { .. } => conflict("run_turn_limit_reached", error),
            ExecutionError::InvalidFinalMessage => conflict("invalid_final_message", error),
            ExecutionError::TooManySupportedRevisions(_)
            | ExecutionError::InvalidMessagePageSize
            | ExecutionError::InvalidQueuedMessagePageSize
            | ExecutionError::InvalidQueuedMessageAfterSequence => {
                ApiError::invalid_request(error.to_string())
            }
            ExecutionError::MessageBatchTooLarge(_) => ApiError::new(
                StatusCode::UNPROCESSABLE_ENTITY,
                "message_batch_too_large",
                error.to_string(),
            ),
            ExecutionError::WaitNotFound(_) => {
                ApiError::new(StatusCode::NOT_FOUND, "wait_not_found", error.to_string())
            }
            ExecutionError::WaitIdConflict(_) => conflict("wait_id_conflict", error),
            ExecutionError::HarnessWaitIdConflict(_) => conflict("harness_wait_id_conflict", error),
            ExecutionError::RunAlreadyWaiting(_) => conflict("run_already_waiting", error),
            ExecutionError::WaitNotPending(_) => conflict("wait_not_pending", error),
            ExecutionError::RunNotWaiting { .. } => conflict("run_not_waiting", error),
            ExecutionError::InvalidWaitPageSize | ExecutionError::InvalidWaitCursor => {
                ApiError::invalid_request(error.to_string())
            }
            ExecutionError::AbortNotFound(_) => {
                ApiError::new(StatusCode::NOT_FOUND, "abort_not_found", error.to_string())
            }
            ExecutionError::AbortIdConflict(_) => conflict("abort_id_conflict", error),
            ExecutionError::AbortInProgress(_) => conflict("abort_in_progress", error),
            ExecutionError::RunNotAbortable { .. } => conflict("run_not_abortable", error),
            ExecutionError::AbortNotPending(_) => conflict("abort_not_pending", error),
            ExecutionError::AbortNotDelivered(_) => conflict("abort_not_delivered", error),
            ExecutionError::RunNotResumable { .. } => conflict("run_not_resumable", error),
            ExecutionError::RunNotLatest(_) => conflict("run_not_latest", error),
            ExecutionError::NoFinalizedAbort(_) => conflict("no_finalized_abort", error),
            ExecutionError::AbortAlreadyResumed(_) => conflict("abort_already_resumed", error),
            ExecutionError::InvalidAbortPageSize | ExecutionError::InvalidAbortCursor => {
                ApiError::invalid_request(error.to_string())
            }
            ExecutionError::InvalidStoredData(_) | ExecutionError::Database(_) => {
                tracing::error!(%error, "execution request failed");
                ApiError::internal()
            }
        }
    }
}

fn conflict(code: &'static str, error: ExecutionError) -> ApiError {
    ApiError::new(StatusCode::CONFLICT, code, error.to_string())
}
