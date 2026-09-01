use axum::{
    Json,
    http::{HeaderValue, StatusCode, header::CACHE_CONTROL},
    response::{IntoResponse, Response},
};
use serde::Serialize;
use serde_json::Value;

use crate::harnesses::HarnessModelOptionsError;
use crate::projects::ProjectError;
use crate::upstream::agent::AgentError;
use crate::upstream::execution_gateway::ExecutionGatewayError;
use crate::upstream::llm_gateway::LlmGatewayError;

#[derive(Debug)]
pub enum ApiError {
    InvalidRequest(String),
    Agent(AgentError),
    ExecutionGateway(ExecutionGatewayError),
    LlmGateway(LlmGatewayError),
    Project(ProjectError),
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

impl From<AgentError> for ApiError {
    fn from(error: AgentError) -> Self {
        Self::Agent(error)
    }
}

impl From<ExecutionGatewayError> for ApiError {
    fn from(error: ExecutionGatewayError) -> Self {
        Self::ExecutionGateway(error)
    }
}

impl From<ProjectError> for ApiError {
    fn from(error: ProjectError) -> Self {
        Self::Project(error)
    }
}

impl From<HarnessModelOptionsError> for ApiError {
    fn from(error: HarnessModelOptionsError) -> Self {
        match error {
            HarnessModelOptionsError::Agent(error) => Self::Agent(error),
            HarnessModelOptionsError::LlmGateway(error) => Self::LlmGateway(error),
        }
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
            Self::Project(ProjectError::InvalidRequest(message)) => (
                StatusCode::BAD_REQUEST,
                Json(ErrorResponse::new("invalid_request", message)),
            )
                .into_response(),
            Self::Project(ProjectError::NotFound) => (
                StatusCode::NOT_FOUND,
                Json(ErrorResponse::new("project_not_found", "project not found")),
            )
                .into_response(),
            Self::Project(ProjectError::HarnessSessionNotFound) => (
                StatusCode::NOT_FOUND,
                Json(ErrorResponse::new(
                    "harness_session_not_found",
                    "project harness session not found",
                )),
            )
                .into_response(),
            Self::Project(ProjectError::HarnessRunNotFound) => (
                StatusCode::NOT_FOUND,
                Json(ErrorResponse::new(
                    "harness_run_not_found",
                    "project harness run not found",
                )),
            )
                .into_response(),
            Self::Project(ProjectError::IdempotencyConflict) => (
                StatusCode::CONFLICT,
                Json(ErrorResponse::new(
                    "idempotency_key_conflict",
                    "the Idempotency-Key is already associated with a different request",
                )),
            )
                .into_response(),
            Self::Project(ProjectError::InvalidMessage(message)) => (
                StatusCode::BAD_REQUEST,
                Json(ErrorResponse::new("invalid_message", message)),
            )
                .into_response(),
            Self::Project(
                error @ (ProjectError::Database(_)
                | ProjectError::InvalidSystemClock
                | ProjectError::RequestSerialization(_)),
            ) => {
                tracing::error!(%error, "project harness session operation failed");
                (
                    StatusCode::INTERNAL_SERVER_ERROR,
                    Json(ErrorResponse::new(
                        "project_store_unavailable",
                        "the project store is unavailable",
                    )),
                )
                    .into_response()
            }
            Self::Agent(AgentError::Rejected { status, body })
            | Self::Project(ProjectError::Agent(AgentError::Rejected { status, body })) => {
                (status, Json(body)).into_response()
            }
            Self::Agent(error) | Self::Project(ProjectError::Agent(error)) => {
                tracing::error!(%error, "Agent request failed");
                (
                    StatusCode::BAD_GATEWAY,
                    Json(ErrorResponse::new(
                        "agent_unavailable",
                        "Agent is unavailable or returned an invalid response",
                    )),
                )
                    .into_response()
            }
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

pub(crate) fn agent_rejected_body(bytes: &[u8]) -> Value {
    serde_json::from_slice(bytes).unwrap_or_else(|_| {
        serde_json::json!({
            "error": {
                "code": "agent_error",
                "message": "Agent rejected the request"
            }
        })
    })
}
