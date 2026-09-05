use axum::{
    Json,
    http::StatusCode,
    response::{IntoResponse, Response},
};
use thiserror::Error;

#[derive(Debug, Error)]
pub enum EnvironmentError {
    #[error("invalid environment request")]
    Invalid(&'static str),
    #[error("invalid environment JSON")]
    Json(StatusCode),
    #[error("resource not found")]
    NotFound,
    #[error("environment changed concurrently")]
    Conflict,
    #[error("gateway reference invalid")]
    Reference(&'static str),
    #[error("gateway unavailable")]
    Gateway,
    #[error("gateway timeout")]
    Timeout,
    #[error("database operation failed")]
    Database(#[from] sqlx::Error),
}

impl IntoResponse for EnvironmentError {
    fn into_response(self) -> Response {
        let (status, code, message) = match self {
            Self::Invalid(message) => (StatusCode::BAD_REQUEST, "INVALID_ENVIRONMENT", message),
            Self::Json(status) => (
                status,
                "INVALID_ENVIRONMENT_JSON",
                "Provide valid JSON containing only supported environment fields.",
            ),
            Self::NotFound => (
                StatusCode::NOT_FOUND,
                "ENVIRONMENT_NOT_FOUND",
                "The project or environment was not found.",
            ),
            Self::Conflict => (
                StatusCode::CONFLICT,
                "ENVIRONMENT_CONFLICT",
                "The environment changed or was deleted. Reload it before trying again.",
            ),
            Self::Reference(message) => (
                StatusCode::UNPROCESSABLE_ENTITY,
                "INVALID_ENVIRONMENT_REFERENCE",
                message,
            ),
            Self::Timeout => (
                StatusCode::GATEWAY_TIMEOUT,
                "EXECUTION_GATEWAY_TIMEOUT",
                "The execution gateway timed out while validating the environment.",
            ),
            Self::Gateway => (
                StatusCode::BAD_GATEWAY,
                "EXECUTION_GATEWAY_ERROR",
                "The execution gateway could not validate the environment. Try again shortly.",
            ),
            Self::Database(error)
                if error
                    .as_database_error()
                    .is_some_and(|e| e.is_foreign_key_violation()) =>
            {
                (
                    StatusCode::NOT_FOUND,
                    "PROJECT_NOT_FOUND",
                    "The project was not found.",
                )
            }
            Self::Database(_) => (
                StatusCode::INTERNAL_SERVER_ERROR,
                "ENVIRONMENT_DATABASE_ERROR",
                "The environment request failed. Reload environments before trying again.",
            ),
        };
        tracing::warn!(%status, code, "environment request failed");
        (
            status,
            Json(serde_json::json!({"error":{"code":code,"message":message}})),
        )
            .into_response()
    }
}
