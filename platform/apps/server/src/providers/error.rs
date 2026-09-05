use axum::{
    Json,
    http::StatusCode,
    response::{IntoResponse, Response},
};
use serde::Serialize;
use thiserror::Error;

#[derive(Debug, Error)]
pub enum ProviderError {
    #[error("provider account not found")]
    NotFound,
    #[error("invalid provider ID")]
    InvalidProviderId,
    #[error("invalid provider input")]
    InvalidRequest(&'static str),
    #[error("invalid JSON payload")]
    InvalidJson(StatusCode),
    #[error("login unavailable")]
    LoginUnavailable,
    #[error("login expired or missing")]
    LoginNotFound,
    #[error("login already exchanging")]
    LoginExchanging,
    #[error("too many login attempts")]
    LoginLimit,
    #[error("LLM gateway request failed")]
    Request(#[source] reqwest::Error),
    #[error("LLM gateway returned HTTP {0}")]
    GatewayStatus(StatusCode),
    #[error("LLM gateway returned an invalid account list")]
    InvalidResponse,
    #[error("LLM gateway response exceeded the size limit")]
    ResponseTooLarge,
}

impl IntoResponse for ProviderError {
    fn into_response(self) -> Response {
        let (status, code, message) = match &self {
            Self::NotFound => (
                StatusCode::NOT_FOUND,
                "PROVIDER_NOT_FOUND",
                "The provider account was not found.",
            ),
            Self::InvalidProviderId => (
                StatusCode::BAD_REQUEST,
                "INVALID_PROVIDER_ID",
                "Provider ID must be a valid UUID.",
            ),
            Self::InvalidRequest(message) => (StatusCode::BAD_REQUEST, "INVALID_REQUEST", *message),
            Self::InvalidJson(status) => (
                *status,
                "INVALID_JSON",
                "Provide a valid JSON request with the required fields.",
            ),
            Self::LoginUnavailable => (
                StatusCode::SERVICE_UNAVAILABLE,
                "LOGIN_UNAVAILABLE",
                "ChatGPT sign-in is unavailable. Check that the local callback server is running on port 1455.",
            ),
            Self::LoginNotFound => (
                StatusCode::NOT_FOUND,
                "LOGIN_NOT_FOUND",
                "This sign-in expired or was canceled. Start again.",
            ),
            Self::LoginExchanging => (
                StatusCode::CONFLICT,
                "LOGIN_EXCHANGING",
                "Sign-in is being completed. Please wait before closing.",
            ),
            Self::LoginLimit => (
                StatusCode::TOO_MANY_REQUESTS,
                "LOGIN_LIMIT",
                "Too many pending sign-ins. Try again later.",
            ),
            Self::Request(error) if error.is_timeout() => (
                StatusCode::GATEWAY_TIMEOUT,
                "LLM_GATEWAY_TIMEOUT",
                "The LLM gateway timed out. Try again shortly.",
            ),
            Self::GatewayStatus(StatusCode::GATEWAY_TIMEOUT) => (
                StatusCode::GATEWAY_TIMEOUT,
                "LLM_GATEWAY_TIMEOUT",
                "The LLM gateway timed out. Try again shortly.",
            ),
            Self::GatewayStatus(StatusCode::TOO_MANY_REQUESTS) => (
                StatusCode::SERVICE_UNAVAILABLE,
                "LLM_GATEWAY_RATE_LIMITED",
                "The LLM gateway is temporarily rate limited. Try again shortly.",
            ),
            _ => (
                StatusCode::BAD_GATEWAY,
                "LLM_GATEWAY_ERROR",
                "The LLM gateway request failed. Refresh the providers list before trying again.",
            ),
        };
        // Do not log the source error, URL, authorization header, or response body.
        tracing::warn!(%status, code, "provider account request failed");
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
struct ErrorResponse {
    error: ErrorBody,
}
#[derive(Serialize)]
struct ErrorBody {
    code: &'static str,
    message: &'static str,
}
