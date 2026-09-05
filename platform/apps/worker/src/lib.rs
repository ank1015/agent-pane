//! Process supervision only: harnesses own execution, recovery and durable commits.
mod supervisor;
// Preserve existing imports; harness libraries depend on harness-runtime directly.
pub use harness_runtime::{Execution, Harness, Signals};
pub use supervisor::{Settings, Snapshot, Supervisor};

use platform_runtime_client::Result;
use std::{collections::BTreeMap, sync::Arc};

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
