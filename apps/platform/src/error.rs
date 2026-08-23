use axum::{
    Json,
    http::{HeaderValue, StatusCode, header::CACHE_CONTROL},
    response::{IntoResponse, Response},
};
use serde::Serialize;
use serde_json::Value;

use crate::upstream::execution_gateway::ExecutionGatewayError;
use crate::upstream::llm_gateway::LlmGatewayError;

#[derive(Debug)]
pub enum ApiError {
    InvalidRequest(String),
    ExecutionGateway(ExecutionGatewayError),
    LlmGateway(LlmGatewayError),
}

impl ApiError {
    pub fn invalid_request(message: impl Into<String>) -> Self {
        Self::InvalidRequest(message.into())
    }
}

impl From<LlmGatewayError> for ApiError {
    fn from(error: LlmGatewayError) -> Self {
        Self::LlmGateway(error)
    }
}

impl From<ExecutionGatewayError> for ApiError {
    fn from(error: ExecutionGatewayError) -> Self {
        Self::ExecutionGateway(error)
    }
}

impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        let response = match self {
            Self::InvalidRequest(message) => (
                StatusCode::BAD_REQUEST,
                Json(ErrorResponse::new("invalid_request", message)),
            )
                .into_response(),
            Self::ExecutionGateway(ExecutionGatewayError::Rejected { status, body }) => {
                (status, Json(body)).into_response()
            }
            Self::ExecutionGateway(error) => {
                tracing::error!(%error, "execution gateway request failed");
                (
                    StatusCode::BAD_GATEWAY,
                    Json(ErrorResponse::new(
                        "execution_gateway_unavailable",
                        "the execution gateway is unavailable or returned an invalid response",
                    )),
                )
                    .into_response()
            }
            Self::LlmGateway(LlmGatewayError::Rejected { status, body }) => {
                (status, Json(body)).into_response()
            }
            Self::LlmGateway(error) => {
                tracing::error!(%error, "llm gateway request failed");
                (
                    StatusCode::BAD_GATEWAY,
                    Json(ErrorResponse::new(
                        "llm_gateway_unavailable",
                        "the LLM gateway is unavailable or returned an invalid response",
                    )),
                )
                    .into_response()
            }
        };

        with_no_store(response)
    }
}

fn with_no_store(mut response: Response) -> Response {
    response
        .headers_mut()
        .insert(CACHE_CONTROL, HeaderValue::from_static("no-store"));
    response
}

#[derive(Serialize)]
struct ErrorResponse {
    error: ErrorBody,
}

impl ErrorResponse {
    fn new(code: &'static str, message: impl Into<String>) -> Self {
        Self {
            error: ErrorBody {
                code,
                message: message.into(),
            },
        }
    }
}

#[derive(Serialize)]
struct ErrorBody {
    code: &'static str,
    message: String,
}

pub(crate) fn rejected_body(bytes: &[u8]) -> Value {
    serde_json::from_slice(bytes).unwrap_or_else(|_| {
        serde_json::json!({
            "error": {
                "code": "llm_gateway_error",
                "message": "the LLM gateway rejected the request"
            }
        })
    })
}

pub(crate) fn execution_gateway_rejected_body(bytes: &[u8]) -> Value {
    serde_json::from_slice(bytes).unwrap_or_else(|_| {
        serde_json::json!({
            "code": "internal",
            "message": "the execution gateway rejected the request",
            "retryable": false
        })
    })
}
