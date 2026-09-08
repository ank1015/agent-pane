//! Shared harness interface, independent of worker registration and supervision.
#![doc = include_str!("../README.md")]

use futures_util::future::BoxFuture;
use platform_runtime_client::{Result, RunClient};
use tokio::sync::watch;
mod workspace;
pub use workspace::publish_workspace;

/// Signals are hints, not acknowledged inputs or optimistic concurrency tokens.
#[derive(Clone, Copy, Debug, Default)]
pub struct Signals {
    pub abort_requested: bool,
    pub draining: bool,
    pub ownership_lost: bool,
    pub input_generation: u64,
}

/// A lease-scoped client and cooperative signals supplied by the worker.
pub struct Execution {
    /// Lease-scoped writes and `queries()` for shared, read-only history access.
    pub client: RunClient,
    pub signals: watch::Receiver<Signals>,
}

/// A recoverable activation, not a model turn. Read context/checkpoint and all
/// required pages through the client. Commit before returning to release the
/// lease (ready/waiting/terminal). Errors/panics do not fabricate terminal state.
/// Persist commands before external effects. Never detach execution tasks; CPU
/// work must yield or use bounded blocking work with its own cancellation.
pub trait Harness: Send + Sync {
    fn run(&self, execution: Execution) -> BoxFuture<'static, Result<()>>;
}
