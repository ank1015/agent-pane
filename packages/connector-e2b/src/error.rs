use std::collections::BTreeMap;

use execution_contracts::{ExecutionError, ExecutionErrorCode};
use thiserror::Error;

#[derive(Debug, Error)]
pub enum E2bConnectorError {
    #[error("E2B connector configuration is invalid: {0}")]
    InvalidConfiguration(String),
    #[error(transparent)]
    Transport(#[from] E2bTransportError),
}

#[derive(Clone, Debug, Error)]
#[error("{message}")]
pub struct E2bTransportError {
    pub message: String,
    pub retryable: bool,
    pub disconnected: bool,
}

impl E2bTransportError {
    pub fn new(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
            retryable: false,
            disconnected: false,
        }
    }

    pub fn disconnected(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
            retryable: true,
            disconnected: true,
        }
    }
}

pub(crate) fn execution_error(
    code: ExecutionErrorCode,
    message: impl Into<String>,
) -> ExecutionError {
    ExecutionError {
        code,
        message: message.into(),
        retryable: false,
        details: BTreeMap::new(),
    }
}

pub(crate) fn transport_execution_error(source: E2bTransportError) -> ExecutionError {
    ExecutionError {
        code: if source.disconnected {
            ExecutionErrorCode::Disconnected
        } else {
            ExecutionErrorCode::Internal
        },
        message: source.message,
        retryable: source.retryable,
        details: BTreeMap::new(),
    }
}

pub(crate) fn invalid_request(error: impl std::fmt::Display) -> ExecutionError {
    execution_error(ExecutionErrorCode::InvalidRequest, error.to_string())
}
