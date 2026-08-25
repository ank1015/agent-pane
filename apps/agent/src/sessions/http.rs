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
use uuid::Uuid;

use super::{CreateSession, SessionError, SessionMessageListQuery, SessionRunListQuery, service};
use crate::{api_error::ApiError, app::AppState};

pub fn router() -> Router<AppState> {
    Router::new()
        .route("/v1/sessions", post(create_session))
        .route("/v1/sessions/{session_id}", get(get_session))
        .route("/v1/sessions/{session_id}/messages", get(list_messages))
        .route("/v1/sessions/{session_id}/runs", get(list_runs))
}

async fn create_session(
    State(state): State<AppState>,
    payload: Result<Json<CreateSession>, JsonRejection>,
) -> Result<Response, ApiError> {
    let outcome = service::create(state.database(), json_body(payload)?).await?;
    let status = if outcome.created {
        StatusCode::CREATED
    } else {
        StatusCode::OK
    };
    Ok(json_response(status, outcome.value))
}

async fn get_session(
    State(state): State<AppState>,
    path: Result<Path<String>, PathRejection>,
) -> Result<Response, ApiError> {
    let session = service::get(state.database(), session_path(path)?).await?;
    Ok(json_response(StatusCode::OK, session))
}

async fn list_messages(
    State(state): State<AppState>,
    path: Result<Path<String>, PathRejection>,
    query: Result<Query<SessionMessageListQuery>, QueryRejection>,
) -> Result<Response, ApiError> {
    let page =
        service::list_messages(state.database(), session_path(path)?, query_params(query)?).await?;
    Ok(json_response(StatusCode::OK, page))
}

async fn list_runs(
    State(state): State<AppState>,
    path: Result<Path<String>, PathRejection>,
    query: Result<Query<SessionRunListQuery>, QueryRejection>,
) -> Result<Response, ApiError> {
    let page =
        service::list_runs(state.database(), session_path(path)?, query_params(query)?).await?;
    Ok(json_response(StatusCode::OK, page))
}

fn json_body<T>(payload: Result<Json<T>, JsonRejection>) -> Result<T, ApiError> {
    payload
        .map(|Json(value)| value)
        .map_err(|error| ApiError::invalid_request(error.body_text()))
}

fn query_params<T>(query: Result<Query<T>, QueryRejection>) -> Result<T, ApiError> {
    query
        .map(|Query(value)| value)
        .map_err(|error| ApiError::invalid_request(error.body_text()))
}

fn session_path(path: Result<Path<String>, PathRejection>) -> Result<Uuid, ApiError> {
    let Path(session_id) = path.map_err(|error| ApiError::invalid_request(error.body_text()))?;
    Uuid::parse_str(&session_id).map_err(|_| ApiError::invalid_request("session_id must be a UUID"))
}

fn json_response<T: Serialize>(status: StatusCode, body: T) -> Response {
    let mut response = (status, Json(body)).into_response();
    response
        .headers_mut()
        .insert(CACHE_CONTROL, HeaderValue::from_static("no-store"));
    response
}

impl From<SessionError> for ApiError {
    fn from(error: SessionError) -> Self {
        match error {
            SessionError::NotFound(_) => ApiError::new(
                StatusCode::NOT_FOUND,
                "session_not_found",
                error.to_string(),
            ),
            SessionError::InvalidMessagePageSize
            | SessionError::InvalidRunPageSize
            | SessionError::InvalidCursor
            | SessionError::InvalidAfterRevision => ApiError::invalid_request(error.to_string()),
            SessionError::InvalidStoredData(_) | SessionError::Database(_) => {
                tracing::error!(%error, "session request failed");
                ApiError::internal()
            }
        }
    }
}
