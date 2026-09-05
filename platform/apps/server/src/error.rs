use axum::{
    Json,
    http::StatusCode,
    response::{IntoResponse, Response},
};
use serde::Serialize;
use thiserror::Error;

#[derive(Debug, Error)]
pub enum ApiError {
    #[error("{0}")]
    InvalidRequest(&'static str),
    #[error("invalid JSON request")]
    InvalidJson(StatusCode),
    #[error("E2B account creation was rejected")]
    AccountRejected(StatusCode),
    #[error("the execution gateway rejected the host {0}")]
    HostRejected(&'static str, StatusCode),
    #[error("the execution gateway could not be reached")]
    GatewayUnavailable(#[source] reqwest::Error),
    #[error("the execution gateway returned HTTP {0}")]
    GatewayStatus(StatusCode),
    #[error("the execution gateway returned an invalid response")]
    InvalidGatewayResponse(#[source] reqwest::Error),
}

impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        let (status, code, message) = match &self {
            Self::InvalidRequest(message) => (StatusCode::BAD_REQUEST, "INVALID_REQUEST", *message),
            Self::InvalidJson(status) => (
                *status,
                "INVALID_REQUEST",
                "Send a valid JSON request within the 16 KiB request limit.",
            ),
            Self::HostRejected(_, StatusCode::NOT_FOUND) => (
                StatusCode::NOT_FOUND,
                "MACHINE_NOT_FOUND",
                "The machine was not found or was already deleted.",
            ),
            Self::HostRejected("update", StatusCode::BAD_REQUEST) => (
                StatusCode::BAD_REQUEST,
                "INVALID_MACHINE",
                "The gateway rejected the machine name.",
            ),
            Self::HostRejected(_, StatusCode::CONFLICT) => (
                StatusCode::CONFLICT,
                "MACHINE_CONFLICT",
                "The machine cannot be changed in its current state.",
            ),
            Self::HostRejected(_, StatusCode::TOO_MANY_REQUESTS) => (
                StatusCode::TOO_MANY_REQUESTS,
                "GATEWAY_RATE_LIMITED",
                "The execution gateway is rate limiting requests. Wait before trying again.",
            ),
            Self::HostRejected(_, StatusCode::GATEWAY_TIMEOUT) => (
                StatusCode::GATEWAY_TIMEOUT,
                "GATEWAY_TIMEOUT",
                "The gateway timed out. Refresh the machine list before retrying; the change may already have completed.",
            ),
            Self::HostRejected(_, _) => (
                StatusCode::BAD_GATEWAY,
                "EXECUTION_GATEWAY_ERROR",
                "The machine request failed. Refresh the machine list before retrying; the change may already have completed.",
            ),
            Self::AccountRejected(StatusCode::UNPROCESSABLE_ENTITY) => (
                StatusCode::UNPROCESSABLE_ENTITY,
                "INVALID_E2B_CREDENTIAL",
                "E2B could not verify this API key. Check the key and try again.",
            ),
            Self::AccountRejected(StatusCode::BAD_REQUEST) => (
                StatusCode::BAD_REQUEST,
                "INVALID_E2B_ACCOUNT",
                "The gateway rejected the account name or API key.",
            ),
            Self::AccountRejected(StatusCode::CONFLICT) => (
                StatusCode::CONFLICT,
                "E2B_ACCOUNT_CONFLICT",
                "The account conflicts with an existing account.",
            ),
            Self::AccountRejected(StatusCode::TOO_MANY_REQUESTS) => (
                StatusCode::TOO_MANY_REQUESTS,
                "E2B_RATE_LIMITED",
                "E2B is rate limiting requests. Wait before trying again.",
            ),
            Self::AccountRejected(StatusCode::GATEWAY_TIMEOUT) => (
                StatusCode::GATEWAY_TIMEOUT,
                "GATEWAY_TIMEOUT",
                "The gateway timed out. Refresh the account list before retrying creation.",
            ),
            Self::GatewayUnavailable(error) if error.is_timeout() => (
                StatusCode::GATEWAY_TIMEOUT,
                "GATEWAY_TIMEOUT",
                "The gateway timed out. Refresh the account list before retrying creation.",
            ),
            _ => (
                StatusCode::BAD_GATEWAY,
                "EXECUTION_GATEWAY_ERROR",
                "The execution gateway request failed. Refresh the account list before retrying creation.",
            ),
        };
        // Log only safe categories, never a response body or decoder source.
        tracing::warn!(%status, code, "platform API request failed");
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
