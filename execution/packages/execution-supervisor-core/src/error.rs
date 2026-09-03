use std::{io, path::Path};

use execution_core::{ExecutionError, ExecutionErrorCode};
use thiserror::Error;

/// Failure to construct a supervisor runtime from local configuration.
#[derive(Debug, Error)]
pub enum SupervisorInitError {
    #[error("invalid supervisor configuration: {0}")]
    InvalidConfiguration(String),
    #[error("supervisor filesystem initialization failed: {0}")]
    Io(#[from] io::Error),
}

pub(crate) fn error(code: ExecutionErrorCode, message: impl Into<String>) -> ExecutionError {
    ExecutionError::new(code, message)
}

pub(crate) fn invalid_request(message: impl Into<String>) -> ExecutionError {
    error(ExecutionErrorCode::InvalidRequest, message)
}

pub(crate) fn operation_conflict(message: impl Into<String>) -> ExecutionError {
    error(ExecutionErrorCode::OperationConflict, message)
}

pub(crate) fn io_error(path: &Path, source: io::Error) -> ExecutionError {
    let code = match source.kind() {
        io::ErrorKind::NotFound => ExecutionErrorCode::NotFound,
        io::ErrorKind::AlreadyExists => ExecutionErrorCode::AlreadyExists,
        io::ErrorKind::PermissionDenied => ExecutionErrorCode::PermissionDenied,
        io::ErrorKind::InvalidInput | io::ErrorKind::InvalidData => {
            ExecutionErrorCode::InvalidRequest
        }
        io::ErrorKind::TimedOut => ExecutionErrorCode::DeadlineExceeded,
        _ => ExecutionErrorCode::Io,
    };
    error(code, format!("{}: {source}", path.display()))
}

pub(crate) fn join_error(message: &str, source: tokio::task::JoinError) -> ExecutionError {
    error(ExecutionErrorCode::Internal, format!("{message}: {source}"))
}
