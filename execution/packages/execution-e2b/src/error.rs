use std::{collections::BTreeMap, fmt};

use execution_core::{ExecutionError, ExecutionErrorCode};
use reqwest::StatusCode;
use thiserror::Error;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum E2bErrorKind {
    Configuration,
    Authentication,
    NotFound,
    RateLimited,
    Unavailable,
    Cancelled,
    DeadlineExceeded,
    Protocol,
    Remote,
    Io,
}

#[derive(Clone, Debug, Error)]
#[error("{message}")]
pub struct E2bError {
    pub kind: E2bErrorKind,
    pub message: String,
    /// The exact operation may be replayed without changing its semantics.
    pub retryable: bool,
    /// The request may have succeeded remotely even though no response arrived.
    pub outcome_ambiguous: bool,
    pub status: Option<u16>,
}

impl E2bError {
    pub(crate) fn configuration(message: impl Into<String>) -> Self {
        Self::new(E2bErrorKind::Configuration, message)
    }

    pub(crate) fn protocol(message: impl Into<String>) -> Self {
        Self::new(E2bErrorKind::Protocol, message)
    }

    pub(crate) fn unavailable(message: impl Into<String>) -> Self {
        Self::new(E2bErrorKind::Unavailable, message).retryable(true)
    }

    pub(crate) fn new(kind: E2bErrorKind, message: impl Into<String>) -> Self {
        Self {
            kind,
            message: message.into(),
            retryable: false,
            outcome_ambiguous: false,
            status: None,
        }
    }

    pub(crate) fn retryable(mut self, value: bool) -> Self {
        self.retryable = value;
        self
    }

    pub(crate) fn ambiguous(mut self, value: bool) -> Self {
        self.outcome_ambiguous = value;
        self
    }

    pub(crate) fn with_status(mut self, status: StatusCode) -> Self {
        self.status = Some(status.as_u16());
        self
    }

    pub(crate) fn from_request(error: reqwest::Error, operation_is_replayable: bool) -> Self {
        let transient = error.is_timeout() || error.is_connect() || error.is_body();
        Self::new(
            if transient {
                E2bErrorKind::Unavailable
            } else {
                E2bErrorKind::Remote
            },
            format!("E2B request failed: {error}"),
        )
        .retryable(transient && operation_is_replayable)
        .ambiguous(!operation_is_replayable)
    }

    pub(crate) fn from_response(
        status: StatusCode,
        body: &str,
        operation_is_replayable: bool,
    ) -> Self {
        let transient = status == StatusCode::TOO_MANY_REQUESTS
            || matches!(status.as_u16(), 408 | 425 | 502 | 503 | 504)
            || status.is_server_error();
        let kind = match status {
            StatusCode::UNAUTHORIZED | StatusCode::FORBIDDEN => E2bErrorKind::Authentication,
            StatusCode::NOT_FOUND => E2bErrorKind::NotFound,
            StatusCode::TOO_MANY_REQUESTS => E2bErrorKind::RateLimited,
            _ if transient => E2bErrorKind::Unavailable,
            _ => E2bErrorKind::Remote,
        };
        let body = truncate(body, 4096);
        Self::new(kind, format!("E2B returned {status}: {body}"))
            .retryable(transient && operation_is_replayable)
            .ambiguous(transient && !operation_is_replayable)
            .with_status(status)
    }

    pub(crate) fn into_execution(self) -> ExecutionError {
        let code = match self.kind {
            E2bErrorKind::Configuration | E2bErrorKind::Protocol => ExecutionErrorCode::Internal,
            E2bErrorKind::Authentication => ExecutionErrorCode::PermissionDenied,
            E2bErrorKind::NotFound => ExecutionErrorCode::ExecutionLost,
            E2bErrorKind::RateLimited => ExecutionErrorCode::ResourceExhausted,
            E2bErrorKind::Unavailable => ExecutionErrorCode::Unavailable,
            E2bErrorKind::Cancelled => ExecutionErrorCode::Cancelled,
            E2bErrorKind::DeadlineExceeded => ExecutionErrorCode::DeadlineExceeded,
            E2bErrorKind::Remote | E2bErrorKind::Io => ExecutionErrorCode::Io,
        };
        let mut details = BTreeMap::new();
        if let Some(status) = self.status {
            details.insert("e2b_http_status".to_owned(), status.into());
        }
        if self.outcome_ambiguous {
            details.insert("outcome_ambiguous".to_owned(), true.into());
        }
        ExecutionError {
            code,
            message: self.message,
            retryable: self.retryable,
            details,
        }
    }

    pub(crate) fn from_execution(error: ExecutionError) -> Self {
        let kind = match error.code {
            ExecutionErrorCode::Cancelled => E2bErrorKind::Cancelled,
            ExecutionErrorCode::DeadlineExceeded => E2bErrorKind::DeadlineExceeded,
            ExecutionErrorCode::ExecutionLost | ExecutionErrorCode::ExecutionNotFound => {
                E2bErrorKind::NotFound
            }
            ExecutionErrorCode::PermissionDenied => E2bErrorKind::Authentication,
            ExecutionErrorCode::Unavailable => E2bErrorKind::Unavailable,
            _ => E2bErrorKind::Remote,
        };
        Self {
            kind,
            message: error.message,
            retryable: error.retryable,
            outcome_ambiguous: false,
            status: None,
        }
    }
}

pub type E2bResult<T> = Result<T, E2bError>;

fn truncate(value: &str, max_chars: usize) -> String {
    if value.chars().count() <= max_chars {
        value.to_owned()
    } else {
        let mut output: String = value.chars().take(max_chars).collect();
        output.push('…');
        output
    }
}

impl From<std::io::Error> for E2bError {
    fn from(error: std::io::Error) -> Self {
        Self::new(E2bErrorKind::Io, format!("local I/O failed: {error}"))
    }
}

impl From<serde_json::Error> for E2bError {
    fn from(error: serde_json::Error) -> Self {
        Self::protocol(format!("invalid JSON: {error}"))
    }
}

impl From<url::ParseError> for E2bError {
    fn from(error: url::ParseError) -> Self {
        Self::configuration(format!("invalid URL: {error}"))
    }
}

impl From<execution_wire::WireCodecError> for E2bError {
    fn from(error: execution_wire::WireCodecError) -> Self {
        Self::protocol(error.to_string())
    }
}

impl From<ExecutionError> for E2bError {
    fn from(error: ExecutionError) -> Self {
        Self::from_execution(error)
    }
}

impl fmt::Display for E2bErrorKind {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "{self:?}")
    }
}
