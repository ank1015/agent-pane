use llm_contracts::JsonObject;
use serde::{Deserialize, Serialize};
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
    pub site_id: Option<Uuid>,
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
