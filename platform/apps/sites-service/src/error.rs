use axum::{
    Json,
    http::StatusCode,
    response::{IntoResponse, Response},
};
use serde_json::json;

pub type Result<T> = std::result::Result<T, Error>;

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("invalid configuration: {0}")]
    Config(&'static str),
    #[error("storage is already owned by another sites service")]
    Locked,
    #[error("storage layout is missing, unsafe, or inconsistent")]
    Storage,
    #[error("site not found")]
    NotFound,
    #[error("site already belongs to a different project")]
    ProjectConflict,
    #[error("invalid request: {0}")]
    Invalid(&'static str),
    #[error("unauthorized")]
    Unauthorized,
    #[error("{1}")]
    Conflict(&'static str, &'static str),
    #[error("bundle or asset not found")]
    ArtifactNotFound,
    #[error("invalid JSON body")]
    Body(StatusCode),
    #[error("service is at capacity")]
    Capacity,
    #[error("storage I/O failed")]
    Io(#[from] std::io::Error),
    #[error("database operation failed")]
    Database(#[from] sqlx::Error),
    #[error("service migration failed")]
    Migration(#[from] sqlx::migrate::MigrateError),
    #[error("storage task failed")]
    Task(#[from] tokio::task::JoinError),
}

impl IntoResponse for Error {
    fn into_response(self) -> Response {
        let (status, code, message) = match self {
            Self::NotFound => (StatusCode::NOT_FOUND, "SITE_NOT_FOUND", "Site not found."),
            Self::ProjectConflict => (
                StatusCode::CONFLICT,
                "SITE_PROJECT_CONFLICT",
                "Site already belongs to a different project.",
            ),
            Self::Invalid(message) => (StatusCode::BAD_REQUEST, "INVALID_REQUEST", message),
            Self::Body(status) => (
                status,
                "INVALID_BODY",
                "Provide a valid JSON object with only the documented fields.",
            ),
            Self::Capacity => (
                StatusCode::SERVICE_UNAVAILABLE,
                "CAPACITY_EXCEEDED",
                "Too many concurrent operations.",
            ),
            Self::Conflict(code, message) => (StatusCode::CONFLICT, code, message),
            Self::ArtifactNotFound => (
                StatusCode::NOT_FOUND,
                "ARTIFACT_NOT_FOUND",
                "Bundle or asset not found.",
            ),
            Self::Unauthorized => (
                StatusCode::UNAUTHORIZED,
                "UNAUTHORIZED",
                "Valid internal authentication is required.",
            ),
            _ => {
                // Do not return filesystem paths, SQL, or underlying errors.
                tracing::error!(error = %self, "sites service storage operation failed");
                (
                    StatusCode::SERVICE_UNAVAILABLE,
                    "STORAGE_UNAVAILABLE",
                    "Site storage is unavailable. Retry with the same site identity.",
                )
            }
        };
        (
            status,
            Json(json!({"error": {"code": code, "message": message}})),
        )
            .into_response()
    }
}
