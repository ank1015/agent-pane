mod cursor;
mod definitions;
mod http;
mod records;
mod revisions;
mod types;

pub use http::router;
pub use types::*;

const DEFAULT_PAGE_SIZE: u32 = 50;
const MAX_PAGE_SIZE: u32 = 100;

pub(super) struct CreateOutcome<T> {
    pub value: T,
    pub created: bool,
}

#[derive(Debug, thiserror::Error)]
pub enum HarnessError {
    #[error(transparent)]
    Validation(#[from] ValidationError),
    #[error("harness {0:?} was not found")]
    HarnessNotFound(String),
    #[error("harness revision {0:?} was not found")]
    RevisionNotFound(String),
    #[error("harness id {0:?} is already used by another harness")]
    HarnessIdConflict(String),
    #[error("harness slug {0:?} is already in use")]
    HarnessSlugConflict(String),
    #[error("revision id {0:?} is already used by another revision")]
    RevisionIdConflict(String),
    #[error("revision {revision:?} already exists for harness {harness_id:?}")]
    RevisionNameConflict {
        harness_id: String,
        revision: String,
    },
    #[error("the harness must have an active revision before it can be enabled")]
    NoActiveRevision,
    #[error("disable the harness before clearing its active revision")]
    EnabledHarnessCannotClearRevision,
    #[error("the active revision cannot be retired")]
    ActiveRevisionCannotBeRetired,
    #[error("a retired revision cannot be activated")]
    RetiredRevisionCannotBeActivated,
    #[error("the page limit must be between 1 and {MAX_PAGE_SIZE}")]
    InvalidPageSize,
    #[error("the pagination cursor is invalid")]
    InvalidCursor,
    #[error("the configuration schema is invalid: {0}")]
    InvalidConfigSchema(String),
    #[error("the default configuration does not satisfy config_schema: {0}")]
    DefaultConfigInvalid(String),
    #[error("stored harness data is invalid: {0}")]
    InvalidStoredData(String),
    #[error(transparent)]
    Database(#[from] sqlx::Error),
}

fn page_limit(requested: Option<u32>) -> Result<u32, HarnessError> {
    let limit = requested.unwrap_or(DEFAULT_PAGE_SIZE);
    if limit == 0 || limit > MAX_PAGE_SIZE {
        return Err(HarnessError::InvalidPageSize);
    }
    Ok(limit)
}

fn constraint(error: &sqlx::Error) -> Option<&str> {
    error
        .as_database_error()
        .and_then(|error| error.constraint())
}
