use crate::tools::{Plan, ToolPolicy};
use llm_contracts::JsonObject;
use serde::{Deserialize, Serialize};
use uuid::Uuid;

/// History is referenced by revision, never duplicated in the checkpoint.
#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct State {
    pub version: u32,
    pub instructions: String,
    pub provider_options: JsonObject,
    pub tools: Vec<llm_contracts::ToolDefinition>,
    pub policy: ToolPolicy,
    pub phase: Phase,
    pub failure: Option<Failure>,
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

#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct Failure {
    pub kind: String,
    pub message: String,
}
