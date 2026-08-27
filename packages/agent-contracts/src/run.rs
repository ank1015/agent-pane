use chrono::{DateTime, Utc};
use llm_contracts::JsonObject;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use uuid::Uuid;

use crate::RunStatus;

/// Durable product state for a harness run.
#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
pub struct Run {
    pub run_id: Uuid,
    pub session_id: Uuid,
    pub trigger_message_id: Uuid,
    pub harness_revision_id: String,
    pub resolved_config: JsonObject,
    pub status: RunStatus,
    pub current_turn: u32,
    pub max_turns: u32,
    pub state_version: u64,
    pub final_message_id: Option<Uuid>,
    pub failure: Option<Value>,
    pub created_at: DateTime<Utc>,
    pub activated_at: DateTime<Utc>,
    pub finished_at: Option<DateTime<Utc>>,
}
