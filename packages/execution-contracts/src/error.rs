use std::collections::BTreeMap;

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use thiserror::Error;

/// Stable category for a failed execution operation.
#[derive(Clone, Copy, Debug, Deserialize, Eq, JsonSchema, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ExecutionErrorCode {
    InvalidRequest,
    InvalidPath,
    Unsupported,
    PermissionDenied,
    NotFound,
    AlreadyExists,
    NotDirectory,
    IsDirectory,
    Conflict,
    StaleRevision,
    ResourceExhausted,
    DeadlineExceeded,
    Cancelled,
    Disconnected,
    UnknownExecution,
    StdinClosed,
    SandboxDenied,
    Internal,
}

/// Transport-neutral error returned by an execution environment.
#[derive(Clone, Debug, Deserialize, Error, JsonSchema, PartialEq, Serialize)]
#[error("{message}")]
pub struct ExecutionError {
    pub code: ExecutionErrorCode,
    pub message: String,
    #[serde(default)]
    pub retryable: bool,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub details: BTreeMap<String, serde_json::Value>,
}

/// Result of one item in a batch where sibling operations may still succeed.
#[derive(Clone, Debug, Deserialize, JsonSchema, PartialEq, Serialize)]
#[serde(tag = "status", rename_all = "snake_case")]
pub enum ItemOutcome<T> {
    Success { value: T },
    Error { error: ExecutionError },
}
