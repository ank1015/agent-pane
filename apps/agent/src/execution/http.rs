use std::{collections::VecDeque, convert::Infallible, time::Duration};

use axum::{
    Json, Router,
    extract::{
        Path, Query, State,
        rejection::{JsonRejection, PathRejection, QueryRejection},
    },
    http::{HeaderMap, HeaderValue, StatusCode, header::CACHE_CONTROL},
    response::{
        IntoResponse, Response,
        sse::{Event, KeepAlive, Sse},
    },
    routing::{get, post},
};
use futures_util::stream;
use serde::{Deserialize, Serialize};
use serde_json::json;
use tokio::sync::broadcast;
use uuid::Uuid;

use super::{
    AbortListQuery, AppendSessionMessages, ExecutionError, HarnessMessageListQuery,
    QueueRunMessage, QueuedRunMessageListQuery, RequestRunAbort, ResolveRunWait, RunEventListQuery,
    StartRun, WaitListQuery, aborts, events, messages, runs, waits,
};
use crate::{api_error::ApiError, app::AppState, db::Database};

pub fn router() -> Router<AppState> {
    Router::new()
        .route("/v1/sessions/{session_id}/runs", post(start_run))
        .route("/v1/runs/{run_id}", get(get_run))
        .route("/v1/runs/{run_id}/events", get(list_run_events))
        .route("/v1/runs/{run_id}/events/stream", get(stream_run_events))
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
async fn list_run_events(
    State(state): State<AppState>,
    path: Result<Path<String>, PathRejection>,
    query: Result<Query<RunEventListQuery>, QueryRejection>,
) -> Result<Response, ApiError> {
    Ok(json_response(
        StatusCode::OK,
        events::list(
            state.database(),
            uuid_path("run_id", path)?,
            query_body(query)?,
        )
        .await?,
    ))
}

#[derive(Clone, Copy, Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
struct RunEventStreamQuery {
    after_sequence: Option<u64>,
}

async fn stream_run_events(
    State(state): State<AppState>,
    path: Result<Path<String>, PathRejection>,
    headers: HeaderMap,
    query: Result<Query<RunEventStreamQuery>, QueryRejection>,
) -> Result<Response, ApiError> {
    let run_id = uuid_path("run_id", path)?;
    runs::get(state.database(), run_id).await?;
    let query = query_body(query)?;
    let after_sequence = match headers.get("last-event-id") {
        Some(value) => value
            .to_str()
            .map_err(|_| ApiError::invalid_request("Last-Event-ID must be an integer"))?
            .parse::<u64>()
            .map_err(|_| ApiError::invalid_request("Last-Event-ID must be an integer"))?,
        None => query.after_sequence.unwrap_or(0),
    };
    i64::try_from(after_sequence).map_err(|_| ExecutionError::InvalidRunEventAfterSequence)?;
    let stream = stream::unfold(
        RunEventStreamState {
            database: state.database().clone(),
            notifications: state.database().subscribe_run_events(),
            run_id,
            after_sequence,
            pending: VecDeque::new(),
            done: false,
        },
        next_stream_event,
    );
    let mut response = Sse::new(stream)
        .keep_alive(
            KeepAlive::new()
                .interval(Duration::from_secs(15))
                .text("keep-alive"),
        )
        .into_response();
    response.headers_mut().insert(
        CACHE_CONTROL,
        HeaderValue::from_static("no-cache, no-transform"),
    );
    response
        .headers_mut()
        .insert("x-accel-buffering", HeaderValue::from_static("no"));
    Ok(response)
}

struct RunEventStreamState {
    database: Database,
    notifications: broadcast::Receiver<Uuid>,
    run_id: Uuid,
    after_sequence: u64,
    pending: VecDeque<agent_contracts::RunEvent>,
    done: bool,
}

async fn next_stream_event(
    mut state: RunEventStreamState,
) -> Option<(Result<Event, Infallible>, RunEventStreamState)> {
    loop {
        if state.done {
            return None;
        }
        if let Some(run_event) = state.pending.pop_front() {
            state.after_sequence = run_event.sequence;
            state.done = run_event.terminal();
            let data = match serde_json::to_string(&run_event) {
                Ok(data) => data,
                Err(error) => {
                    tracing::error!(%error, run_id = %state.run_id, "could not serialize run event for SSE");
                    return None;
                }
            };
            let event = Event::default()
                .id(run_event.sequence.to_string())
                .event(run_event.sse_name())
                .data(data);
            return Some((Ok(event), state));
        }

        let page = match events::list(
            &state.database,
            state.run_id,
            RunEventListQuery {
                after_sequence: Some(state.after_sequence),
                limit: Some(500),
            },
        )
        .await
        {
            Ok(page) => page,
            Err(error) => {
                tracing::warn!(%error, run_id = %state.run_id, "run event SSE query failed");
                return None;
            }
        };
        if !page.items.is_empty() {
            state.pending.extend(page.items);
            continue;
        }
        match runs::get(&state.database, state.run_id).await {
            Ok(run)
                if matches!(
                    run.status,
                    agent_contracts::RunStatus::Aborted
                        | agent_contracts::RunStatus::Completed
                        | agent_contracts::RunStatus::Failed
                ) =>
            {
                return None;
            }
            Ok(_) => {}
            Err(error) => {
                tracing::warn!(%error, run_id = %state.run_id, "could not inspect run while streaming events");
                return None;
            }
        }

        loop {
            match tokio::time::timeout(Duration::from_secs(2), state.notifications.recv()).await {
                Ok(Ok(notified_run)) if notified_run == state.run_id => break,
                Ok(Ok(_)) => {}
                Ok(Err(broadcast::error::RecvError::Lagged(_))) | Err(_) => break,
                Ok(Err(broadcast::error::RecvError::Closed)) => break,
            }
        }
    }
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
            | ExecutionError::InvalidRunEventPageSize
            | ExecutionError::InvalidRunEventAfterSequence
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
        ExecutionError::InvalidCancellationAppend(_) => "invalid_cancellation_append",
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
