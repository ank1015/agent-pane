use std::collections::BTreeMap;

use axum::{
    Json,
    http::StatusCode,
    response::{IntoResponse, Response},
};
use execution_api::{ApiError, ApiErrorBody};
use execution_e2b::{E2bError, E2bErrorKind};
use serde_json::Value;

#[derive(Debug)]
pub struct GatewayError {
    pub status: StatusCode,
    pub code: &'static str,
    pub message: String,
    pub retryable: bool,
    pub details: BTreeMap<String, Value>,
}

impl GatewayError {
    pub fn new(status: StatusCode, code: &'static str, message: impl Into<String>) -> Self {
        Self {
            status,
            code,
            message: message.into(),
            retryable: false,
            details: BTreeMap::new(),
        }
    }

    pub fn bad_request(code: &'static str, message: impl Into<String>) -> Self {
        Self::new(StatusCode::BAD_REQUEST, code, message)
    }

    pub fn not_found(resource: &'static str) -> Self {
        Self::new(
            StatusCode::NOT_FOUND,
            "NOT_FOUND",
            format!("{resource} was not found"),
        )
    }

    pub fn conflict(code: &'static str, message: impl Into<String>) -> Self {
        Self::new(StatusCode::CONFLICT, code, message)
    }

    pub fn internal(message: impl Into<String>) -> Self {
        Self::new(StatusCode::INTERNAL_SERVER_ERROR, "INTERNAL", message)
    }

    pub fn retryable(mut self, value: bool) -> Self {
        self.retryable = value;
        self
    }
}

impl IntoResponse for GatewayError {
    fn into_response(self) -> Response {
        let body = ApiErrorBody {
            error: ApiError {
                code: self.code.to_owned(),
                message: self.message,
                retryable: self.retryable,
                details: self.details,
            },
        };
        (self.status, Json(body)).into_response()
    }
}

impl From<sqlx::Error> for GatewayError {
    fn from(error: sqlx::Error) -> Self {
        tracing::error!(%error, "database operation failed");
        Self::internal("database operation failed")
    }
}

impl From<crate::crypto::VaultError> for GatewayError {
    fn from(error: crate::crypto::VaultError) -> Self {
        tracing::error!(%error, "credential vault operation failed");
        Self::internal("credential vault operation failed")
    }
}

pub fn provider_http_error(error: E2bError) -> GatewayError {
    let status = match error.kind {
        E2bErrorKind::Authentication => StatusCode::UNPROCESSABLE_ENTITY,
        E2bErrorKind::NotFound => StatusCode::GONE,
        E2bErrorKind::RateLimited => StatusCode::TOO_MANY_REQUESTS,
        E2bErrorKind::DeadlineExceeded => StatusCode::GATEWAY_TIMEOUT,
        E2bErrorKind::Unavailable => StatusCode::BAD_GATEWAY,
        E2bErrorKind::Configuration | E2bErrorKind::Protocol => StatusCode::BAD_GATEWAY,
        E2bErrorKind::Cancelled => StatusCode::REQUEST_TIMEOUT,
        E2bErrorKind::Remote | E2bErrorKind::Io => StatusCode::BAD_GATEWAY,
    };
    GatewayError::new(status, "E2B_ERROR", error.message).retryable(error.retryable)
}
