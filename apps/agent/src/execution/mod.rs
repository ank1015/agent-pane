mod abort_records;
mod aborting;
mod configuration;
mod finishing;
mod http;
mod leasing;
mod messages;
pub mod model;
mod policy;
mod reaper;
mod records;
mod start;
mod steer;
mod waiting;

pub use http::{router, worker_router};
pub use model::*;
pub use policy::{ExecutionPolicy, ExecutionPolicyError};
pub use reaper::{reap_expired_once, spawn_reaper};

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
    #[error("session {0} already has an active run")]
    SessionHasActiveRun(uuid::Uuid),
    #[error("harness {0:?} is not available for new runs")]
    HarnessNotAvailable(String),
    #[error("harness revision {0:?} is not available for new runs")]
    HarnessRevisionNotAvailable(String),
    #[error("the resolved harness configuration is invalid: {0}")]
    InvalidHarnessConfiguration(String),
    #[error("requested {field} of {requested} exceeds the server maximum of {maximum}")]
    RunLimitExceeded {
        field: &'static str,
        requested: u32,
        maximum: u32,
    },
    #[error("lease id {0} is already in use")]
    LeaseIdConflict(uuid::Uuid),
    #[error("the run lease is no longer active")]
    LeaseLost,
    #[error("the lease token must be unpadded base64url encoding exactly 32 bytes")]
    InvalidLeaseToken,
    #[error("expected run state version {expected}, but the current version is {actual}")]
    RunStateConflict { expected: u64, actual: u64 },
    #[error("run {run_id} is {status:?} and cannot perform this operation")]
    RunNotRunnable {
        run_id: uuid::Uuid,
        status: RunStatus,
    },
    #[error("run {run_id} has reached its turn limit of {max_turns}")]
    RunTurnLimitReached { run_id: uuid::Uuid, max_turns: u32 },
    #[error("the final message is not a harness message from the current run turn")]
    InvalidFinalMessage,
    #[error("the supported harness revision list exceeds the maximum of {0}")]
    TooManySupportedRevisions(usize),
    #[error("the message batch exceeds the maximum of {0}")]
    MessageBatchTooLarge(usize),
    #[error("message page limit must be between 1 and 500")]
    InvalidMessagePageSize,
    #[error("queued run message page limit must be between 1 and 100")]
    InvalidQueuedMessagePageSize,
    #[error("queued run message after_sequence is too large")]
    InvalidQueuedMessageAfterSequence,
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
    #[error("run {run_id} is {status:?} instead of waiting")]
    RunNotWaiting {
        run_id: uuid::Uuid,
        status: RunStatus,
    },
    #[error("wait page limit must be between 1 and 100")]
    InvalidWaitPageSize,
    #[error("the wait pagination cursor is invalid")]
    InvalidWaitCursor,
    #[error("abort {0} was not found")]
    AbortNotFound(uuid::Uuid),
    #[error("abort id {0} is already in use")]
    AbortIdConflict(uuid::Uuid),
    #[error("run {0} already has an abort in progress")]
    AbortInProgress(uuid::Uuid),
    #[error("run {run_id} is {status:?} and cannot be aborted")]
    RunNotAbortable {
        run_id: uuid::Uuid,
        status: RunStatus,
    },
    #[error("abort {0} is no longer pending")]
    AbortNotPending(uuid::Uuid),
    #[error("abort {0} has not been delivered to the worker")]
    AbortNotDelivered(uuid::Uuid),
    #[error("run {run_id} is {status:?} and cannot be resumed")]
    RunNotResumable {
        run_id: uuid::Uuid,
        status: RunStatus,
    },
    #[error("run {0} is not the latest run in its session")]
    RunNotLatest(uuid::Uuid),
    #[error("run {0} has no finalized abort to resume")]
    NoFinalizedAbort(uuid::Uuid),
    #[error("run {0} has already resumed its latest abort")]
    AbortAlreadyResumed(uuid::Uuid),
    #[error("abort page limit must be between 1 and 100")]
    InvalidAbortPageSize,
    #[error("the abort pagination cursor is invalid")]
    InvalidAbortCursor,
    #[error("stored execution data is invalid: {0}")]
    InvalidStoredData(String),
    #[error(transparent)]
    Database(#[from] sqlx::Error),
}

fn constraint(error: &sqlx::Error) -> Option<&str> {
    error
        .as_database_error()
        .and_then(|error| error.constraint())
}
