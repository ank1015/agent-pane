//! Process supervision only: harnesses own execution, recovery and durable commits.
mod supervisor;
pub use supervisor::{Settings, Snapshot, Supervisor};

use futures_util::future::BoxFuture;
use platform_runtime_client::{Result, RunClient};
use std::{collections::BTreeMap, sync::Arc};
use tokio::sync::watch;

/// Signals are hints, not acknowledged inputs or optimistic concurrency tokens.
#[derive(Clone, Copy, Debug, Default)]
pub struct Signals {
    pub abort_requested: bool,
    pub draining: bool,
    pub ownership_lost: bool,
    pub input_generation: u64,
}

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

#[derive(Clone, Default)]
pub struct Registry(BTreeMap<String, Arc<dyn Harness>>);
impl Registry {
    pub fn register(&mut self, id: impl Into<String>, harness: Arc<dyn Harness>) -> Result<()> {
        let id = id.into();
        if !platform_runtime_client::types::is_valid_harness_id(&id)
            || self.0.contains_key(&id)
            || self.0.len() >= 200
        {
            return Err(platform_runtime_client::Error::Invalid(
                "invalid or duplicate harness registration",
            ));
        }
        self.0.insert(id, harness);
        Ok(())
    }
    pub fn ids(&self) -> Vec<String> {
        self.0.keys().cloned().collect()
    }
}
