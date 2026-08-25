mod cursor;
mod http;
mod records;
mod service;
mod types;

pub use http::router;
pub use types::*;

const DEFAULT_MESSAGE_PAGE_SIZE: u32 = 100;
const MAX_MESSAGE_PAGE_SIZE: u32 = 500;
const DEFAULT_RUN_PAGE_SIZE: u32 = 50;
const MAX_RUN_PAGE_SIZE: u32 = 100;

pub(super) struct CreateOutcome<T> {
    pub value: T,
    pub created: bool,
}

#[derive(Debug, thiserror::Error)]
pub enum SessionError {
    #[error("session {0} was not found")]
    NotFound(uuid::Uuid),
    #[error("the message page limit must be between 1 and {MAX_MESSAGE_PAGE_SIZE}")]
    InvalidMessagePageSize,
    #[error("the run page limit must be between 1 and {MAX_RUN_PAGE_SIZE}")]
    InvalidRunPageSize,
    #[error("the pagination cursor is invalid")]
    InvalidCursor,
    #[error("after_revision is too large")]
    InvalidAfterRevision,
    #[error("stored session data is invalid: {0}")]
    InvalidStoredData(String),
    #[error(transparent)]
    Database(#[from] sqlx::Error),
}

fn message_page_limit(requested: Option<u32>) -> Result<u32, SessionError> {
    let limit = requested.unwrap_or(DEFAULT_MESSAGE_PAGE_SIZE);
    if limit == 0 || limit > MAX_MESSAGE_PAGE_SIZE {
        return Err(SessionError::InvalidMessagePageSize);
    }
    Ok(limit)
}

fn run_page_limit(requested: Option<u32>) -> Result<u32, SessionError> {
    let limit = requested.unwrap_or(DEFAULT_RUN_PAGE_SIZE);
    if limit == 0 || limit > MAX_RUN_PAGE_SIZE {
        return Err(SessionError::InvalidRunPageSize);
    }
    Ok(limit)
}
