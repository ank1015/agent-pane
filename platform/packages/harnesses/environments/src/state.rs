use llm_contracts::JsonObject;
use serde::{Deserialize, Serialize};
use tool_write::ObservedFile;
use uuid::Uuid;

use crate::tools::Plan;

/// Private, versioned checkpoint; contains no duplicated session history.
#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct State {
    pub version: u32,
    pub instructions: String,
    pub provider_options: JsonObject,
    pub tools: Vec<llm_contracts::ToolDefinition>,
    pub phase: Phase,
    pub observations: Vec<ObservedFile>,
}

#[derive(Clone, Serialize, Deserialize)]
#[serde(tag = "phase", rename_all = "snake_case")]
pub(crate) enum Phase {
    Boundary {
        final_message: Option<Uuid>,
    },
    Model {
        operation: Uuid,
        revision: i64,
        job: Option<Uuid>,
        attempt: u32,
        retry_at_ms: i64,
    },
    Tools {
        assistant: Uuid,
        index: usize,
        plan: Option<Box<Plan>>,
    },
}

impl State {
    pub fn observe(&mut self, observation: ObservedFile) {
        self.observations
            .retain(|old| old.path != observation.path || old.host_id != observation.host_id);
        self.observations.push(observation);
        // Bounded per-run cache; eviction only requires the model to read again.
        while self.observations.len() > 128
            || serde_json::to_vec(&self.observations).unwrap().len() > 64 * 1024
        {
            self.observations.remove(0);
        }
    }
}
