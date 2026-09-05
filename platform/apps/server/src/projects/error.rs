use axum::{
    Json,
    http::StatusCode,
    response::{IntoResponse, Response},
};
use serde::Serialize;
use thiserror::Error;

#[derive(Debug, Error)]
pub enum ProjectError {
    #[error("invalid project request")]
    InvalidRequest(&'static str),
    #[error("invalid project JSON")]
    InvalidJson(StatusCode),
    #[error("project database operation failed")]
    Database(#[from] sqlx::Error),
}

impl IntoResponse for ProjectError {
    fn into_response(self) -> Response {
        let (status, code, message) = match &self {
            Self::InvalidRequest(message) => (StatusCode::BAD_REQUEST, "INVALID_PROJECT", *message),
            Self::InvalidJson(status) => (
                *status,
                "INVALID_PROJECT_JSON",
                "Provide a JSON object containing only name and an optional avatar.",
            ),
            Self::Database(_) => (
                StatusCode::INTERNAL_SERVER_ERROR,
                "PROJECT_DATABASE_ERROR",
                "Projects are temporarily unavailable. Try again shortly.",
            ),
        };
        // Never log database connection strings, SQL values, or avatar payloads.
        tracing::warn!(%status, code, "project request failed");
        (
            status,
            Json(ErrorResponse {
                error: ErrorBody { code, message },
            }),
        )
            .into_response()
    }
}

#[derive(Serialize)]
struct ErrorResponse<'a> {
    error: ErrorBody<'a>,
}
#[derive(Serialize)]
struct ErrorBody<'a> {
    code: &'static str,
    message: &'a str,
}
