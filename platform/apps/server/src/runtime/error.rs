use axum::{
    Json,
    http::StatusCode,
    response::{IntoResponse, Response},
};
use platform_runtime_contracts::{ConflictCode, ErrorEnvelope, ErrorInfo};

#[derive(Debug, thiserror::Error)]
pub enum RuntimeError {
    #[error(transparent)]
    Environment(#[from] crate::projects::environments::EnvironmentError),
    #[error("administrator authentication failed")]
    AdminUnauthorized,
    #[error("worker authentication failed")]
    Unauthorized,
    #[error("invalid runtime request")]
    Invalid(&'static str),
    #[error("runtime resource not found")]
    NotFound,
    #[error("runtime conflict")]
    Conflict(&'static str),
    #[error("runtime conflict: {0:?}")]
    CodedConflict(ConflictCode, &'static str),
    #[error("invalid harness configuration")]
    Configuration,
    #[error("stored runtime data is invalid")]
    StoredData,
    #[error("request body rejected")]
    Body(StatusCode),
    #[error("database operation failed")]
    Database(#[from] sqlx::Error),
}

impl IntoResponse for RuntimeError {
    fn into_response(self) -> Response {
        let (status, code, message) = match self {
            Self::Environment(error) => return error.into_response(),
            Self::AdminUnauthorized => (
                StatusCode::UNAUTHORIZED,
                "ADMIN_UNAUTHORIZED",
                "Valid administrator credentials are required.",
            ),
            Self::Unauthorized => (
                StatusCode::UNAUTHORIZED,
                "WORKER_UNAUTHORIZED",
                "Valid worker credentials are required.",
            ),
            Self::Invalid(message) => (StatusCode::BAD_REQUEST, "INVALID_RUNTIME_REQUEST", message),
            Self::NotFound => (
                StatusCode::NOT_FOUND,
                "RUNTIME_NOT_FOUND",
                "The requested resource was not found.",
            ),
            Self::Conflict(message) => (StatusCode::CONFLICT, "RUNTIME_CONFLICT", message),
            Self::CodedConflict(code, message) => (StatusCode::CONFLICT, code.as_str(), message),
            Self::Configuration => (
                StatusCode::UNPROCESSABLE_ENTITY,
                "INVALID_HARNESS_CONFIGURATION",
                "The resolved configuration does not match the harness schema.",
            ),
            Self::StoredData => (
                StatusCode::INTERNAL_SERVER_ERROR,
                "INVALID_RUNTIME_DATA",
                "Stored runtime configuration is invalid.",
            ),
            Self::Body(status) => (
                status,
                "INVALID_RUNTIME_BODY",
                "Provide valid JSON containing only supported fields and message types.",
            ),
            Self::Database(error) => {
                let code = error.as_database_error().and_then(|e| e.code());
                match code.as_deref() {
                    Some("23505" | "23514" | "23503") => (
                        StatusCode::CONFLICT,
                        ConflictCode::RuntimeConstraintConflict.as_str(),
                        "The operation conflicts with the current runtime state. Reload and retry.",
                    ),
                    Some("55P03" | "57014" | "40001" | "40P01") => (
                        StatusCode::SERVICE_UNAVAILABLE,
                        "RUNTIME_BUSY",
                        "The runtime is busy. Retry using the same idempotency key.",
                    ),
                    _ => (
                        StatusCode::INTERNAL_SERVER_ERROR,
                        "RUNTIME_DATABASE_ERROR",
                        "The runtime request could not be completed.",
                    ),
                }
            }
        };
        // Never log request/configuration bodies or database error detail.
        tracing::warn!(%status, code, "runtime application request failed");
        (
            status,
            Json(ErrorEnvelope {
                error: ErrorInfo {
                    code: code.into(),
                    message: message.into(),
                },
            }),
        )
            .into_response()
    }
}

pub(super) type Result<T> = std::result::Result<T, RuntimeError>;
