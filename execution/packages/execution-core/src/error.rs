use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};
use serde_json::Value;
use thiserror::Error;

use crate::ValidationError;

/// Stable category for a failed execution operation.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ExecutionErrorCode {
    InvalidRequest,
    InvalidPath,
    PathOutsideRoot,
    RootNotFound,
    ReadOnlyRoot,
    Unsupported,
    PermissionDenied,
    NotFound,
    AlreadyExists,
    NotDirectory,
    IsDirectory,
    RevisionConflict,
    OperationConflict,
    ResourceExhausted,
    DeadlineExceeded,
    Cancelled,
    Unavailable,
    ExecutionNotFound,
    ExecutionLost,
    StdinClosed,
    Io,
    Internal,
}

/// Transport-independent error returned by an execution runtime.
#[derive(Clone, Debug, Deserialize, Error, PartialEq, Serialize)]
#[error("{message}")]
#[serde(deny_unknown_fields)]
pub struct ExecutionError {
    pub code: ExecutionErrorCode,
    pub message: String,
    #[serde(default)]
    pub retryable: bool,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub details: BTreeMap<String, Value>,
}

impl ExecutionError {
    #[must_use]
    pub fn new(code: ExecutionErrorCode, message: impl Into<String>) -> Self {
        Self {
            code,
            message: message.into(),
            retryable: false,
            details: BTreeMap::new(),
        }
    }

    #[must_use]
    pub fn retryable(mut self, retryable: bool) -> Self {
        self.retryable = retryable;
        self
    }

    #[must_use]
    pub fn with_detail(mut self, key: impl Into<String>, value: impl Into<Value>) -> Self {
        self.details.insert(key.into(), value.into());
        self
    }

    #[must_use]
    pub fn cancelled() -> Self {
        Self::new(ExecutionErrorCode::Cancelled, "operation was cancelled")
    }

    #[must_use]
    pub fn deadline_exceeded() -> Self {
        Self::new(
            ExecutionErrorCode::DeadlineExceeded,
            "operation deadline was exceeded",
        )
    }
}

impl From<ValidationError> for ExecutionError {
    fn from(error: ValidationError) -> Self {
        let details = serde_json::to_value(&error.issues).unwrap_or(Value::Null);
        Self::new(
            ExecutionErrorCode::InvalidRequest,
            "execution request failed validation",
        )
        .with_detail("issues", details)
    }
}

pub type ExecutionResult<T> = Result<T, ExecutionError>;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn validation_errors_map_to_invalid_requests() {
        let error = ExecutionError::from(ValidationError::single("path", "is invalid"));
        assert_eq!(error.code, ExecutionErrorCode::InvalidRequest);
        assert!(error.details.contains_key("issues"));
    }
}
