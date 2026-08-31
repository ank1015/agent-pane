mod aborts;
mod commands;
mod configuration;
pub(crate) mod events;
mod http;
mod messages;
mod outbox;
mod records;
mod runs;
mod types;
mod waits;

pub use commands::apply_harness_command;
pub use events::{HarnessEventIngestOutcome, ingest_harness_event};
pub use http::{harness_router, router};
pub use outbox::spawn_outbox_publisher;
pub use types::*;
pub use waits::{expire_waits_once, spawn_wait_expiry};

pub(crate) use records::{AbortRow, QueuedMessageRow, RunContextRow, RunRow, WaitRow};

#[derive(Debug, thiserror::Error)]
pub enum ExecutionError {
    #[error(transparent)]
    Validation(#[from] llm_contracts::ValidationError),
    #[error("session {0} was not found")]
    SessionNotFound(uuid::Uuid),
    #[error("run {0} was not found")]
    RunNotFound(uuid::Uuid),
    #[error("run id {0} is already used by another run")]
    RunIdConflict(uuid::Uuid),
    #[error("session message id {0} is already used by another message")]
    MessageIdConflict(uuid::Uuid),
    #[error("queued run message {0} was not found")]
    QueuedMessageNotFound(uuid::Uuid),
    #[error("expected session revision {expected}, but the current revision is {actual}")]
    SessionRevisionConflict { expected: u64, actual: u64 },
    #[error("expected run state version {expected}, but the current version is {actual}")]
    RunStateConflict { expected: u64, actual: u64 },
    #[error("expected run turn {expected}, but the current turn is {actual}")]
    RunTurnConflict { expected: u32, actual: u32 },
    #[error("session {0} already has an active run")]
    SessionHasActiveRun(uuid::Uuid),
    #[error("harness {0:?} is not available for new runs")]
    HarnessNotAvailable(String),
    #[error("harness revision {0:?} is not available for new runs")]
    HarnessRevisionNotAvailable(String),
    #[error("the resolved harness configuration is invalid: {0}")]
    InvalidHarnessConfiguration(String),
    #[error("requested max_turns of {requested} exceeds the server maximum of {maximum}")]
    RunLimitExceeded { requested: u32, maximum: u32 },
    #[error("run {run_id} is {status:?} and cannot perform this operation")]
    RunNotActive {
        run_id: uuid::Uuid,
        status: RunStatus,
    },
    #[error("run {run_id} has reached its turn limit of {max_turns}")]
    RunTurnLimitReached { run_id: uuid::Uuid, max_turns: u32 },
    #[error("the final message is not a harness message from the current run turn")]
    InvalidFinalMessage,
    #[error("the message batch exceeds the maximum of {0}")]
    MessageBatchTooLarge(usize),
    #[error("invalid post-cancellation tool-result append: {0}")]
    InvalidCancellationAppend(String),
    #[error("message page limit must be between 1 and 500")]
    InvalidMessagePageSize,
    #[error("queued run message page limit must be between 1 and 100")]
    InvalidQueuedMessagePageSize,
    #[error("queued run message after_sequence is too large")]
    InvalidQueuedMessageAfterSequence,
    #[error("run event page limit must be between 1 and 500")]
    InvalidRunEventPageSize,
    #[error("run event after_sequence is too large")]
    InvalidRunEventAfterSequence,
    #[error("wait {0} was not found")]
    WaitNotFound(uuid::Uuid),
    #[error("wait id {0} is already in use")]
    WaitIdConflict(uuid::Uuid),
    #[error("harness wait id {0:?} is already in use for this run")]
    HarnessWaitIdConflict(String),
    #[error("run {0} already has a pending wait")]
    RunAlreadyWaiting(uuid::Uuid),
    #[error("wait {0} is no longer pending")]
    WaitNotPending(uuid::Uuid),
    #[error("wait page limit must be between 1 and 100")]
    InvalidWaitPageSize,
    #[error("the wait pagination cursor is invalid")]
    InvalidWaitCursor,
    #[error("abort {0} was not found")]
    AbortNotFound(uuid::Uuid),
    #[error("abort id {0} is already in use")]
    AbortIdConflict(uuid::Uuid),
    #[error("run {run_id} is {status:?} and cannot be aborted")]
    RunNotAbortable {
        run_id: uuid::Uuid,
        status: RunStatus,
    },
    #[error("abort page limit must be between 1 and 100")]
    InvalidAbortPageSize,
    #[error("the abort pagination cursor is invalid")]
    InvalidAbortCursor,
    #[error("command id {0} was reused with a different payload")]
    CommandIdConflict(uuid::Uuid),
    #[error("stored execution data is invalid: {0}")]
    InvalidStoredData(String),
    #[error(transparent)]
    Database(#[from] sqlx::Error),
}

pub(super) fn constraint(error: &sqlx::Error) -> Option<&str> {
    error
        .as_database_error()
        .and_then(|error| error.constraint())
}
