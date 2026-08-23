use std::{collections::BTreeMap, io, path::Path};

use execution_contracts::{ExecutionError, ExecutionErrorCode};
use thiserror::Error;

#[derive(Debug, Error)]
pub enum LocalExecutionError {
    #[error("local execution configuration is invalid: {0}")]
    InvalidConfiguration(String),
    #[error("local filesystem initialization failed: {0}")]
    Io(#[from] io::Error),
}

pub(crate) fn error(code: ExecutionErrorCode, message: impl Into<String>) -> ExecutionError {
    ExecutionError {
        code,
        message: message.into(),
        retryable: false,
        details: BTreeMap::new(),
    }
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
        _ => ExecutionErrorCode::Internal,
    };
    error(code, format!("{}: {source}", path.display()))
}

pub(crate) fn unsupported(message: impl Into<String>) -> ExecutionError {
    error(ExecutionErrorCode::Unsupported, message)
}
