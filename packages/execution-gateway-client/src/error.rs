use std::collections::BTreeMap;

use execution_contracts::{ExecutionError, ExecutionErrorCode};
use reqwest::StatusCode;

#[derive(Debug, thiserror::Error)]
pub enum ExecutionGatewayClientError {
    #[error("execution gateway configuration is invalid: {0}")]
    InvalidConfiguration(String),
    #[error("execution gateway API token is not a valid HTTP header value")]
    InvalidApiToken(#[source] reqwest::header::InvalidHeaderValue),
    #[error("could not build the execution gateway HTTP client")]
    BuildClient(#[source] reqwest::Error),
    #[error("could not construct an execution gateway URL")]
    InvalidUrl,
    #[error("execution gateway request failed")]
    Request(#[source] reqwest::Error),
    #[error("execution gateway returned invalid JSON")]
    InvalidResponse(#[source] serde_json::Error),
    #[error("execution gateway rejected the request with status {status}")]
    Rejected {
        status: StatusCode,
        error: Option<ExecutionError>,
        body: String,
    },
    #[error("execution gateway returned an invalid machine descriptor: {0}")]
    InvalidDescriptor(String),
    #[error("execution gateway returned an invalid environment: {0}")]
    InvalidEnvironment(String),
}

impl ExecutionGatewayClientError {
    pub(crate) fn into_execution_error(self) -> ExecutionError {
        match self {
            Self::Rejected {
                error: Some(error), ..
            } => error,
            Self::Request(error) if error.is_timeout() => execution_error(
                ExecutionErrorCode::DeadlineExceeded,
                "execution gateway request timed out",
                true,
            ),
            Self::Request(_) => execution_error(
                ExecutionErrorCode::Disconnected,
                "execution gateway request failed",
                true,
            ),
            Self::Rejected { status, body, .. } => execution_error(
                ExecutionErrorCode::Internal,
                format!("execution gateway rejected the request with status {status}: {body}"),
                false,
            ),
            Self::InvalidResponse(_) => execution_error(
                ExecutionErrorCode::Internal,
                "execution gateway returned invalid JSON",
                false,
            ),
            error => execution_error(ExecutionErrorCode::Internal, error.to_string(), false),
        }
    }
}

pub(crate) fn execution_error(
    code: ExecutionErrorCode,
    message: impl Into<String>,
    retryable: bool,
) -> ExecutionError {
    ExecutionError {
        code,
        message: message.into(),
        retryable,
        details: BTreeMap::new(),
    }
}

pub(crate) fn cancelled_error() -> ExecutionError {
    execution_error(
        ExecutionErrorCode::Cancelled,
        "execution operation was cancelled",
        false,
    )
}

pub(crate) fn deadline_error() -> ExecutionError {
    execution_error(
        ExecutionErrorCode::DeadlineExceeded,
        "execution operation deadline exceeded",
        false,
    )
}

pub(crate) fn unexpected_response(expected: &str) -> ExecutionError {
    execution_error(
        ExecutionErrorCode::Internal,
        format!("execution gateway returned an unexpected response; expected {expected}"),
        false,
    )
}

pub(crate) fn unexpected_stream_item(expected: &str) -> ExecutionError {
    execution_error(
        ExecutionErrorCode::Internal,
        format!("execution gateway returned an unexpected stream item; expected {expected}"),
        false,
    )
}
