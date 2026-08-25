use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use uuid::Uuid;

pub use agent_contracts::{
    RunStatus, SessionMessage, SessionMessageDelivery, SessionMessageOrigin, SessionMessagePage,
};

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct Session {
    pub session_id: Uuid,
    pub current_revision: u64,
    pub created_at: DateTime<Utc>,
}

#[derive(Clone, Copy, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CreateSession {
    pub session_id: Uuid,
}

#[derive(Clone, Copy, Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SessionMessageListQuery {
    pub after_revision: Option<u64>,
    pub limit: Option<u32>,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
pub struct SessionRunSummary {
    pub run_id: Uuid,
    pub session_id: Uuid,
    pub trigger_message_id: Uuid,
    pub harness_revision_id: String,
    pub status: RunStatus,
    pub current_turn: u32,
    pub max_turns: u32,
    pub failures_in_current_turn: u32,
    pub max_failures_per_turn: u32,
    pub state_version: u64,
    pub final_message_id: Option<Uuid>,
    pub failure: Option<Value>,
    pub queued_at: Option<DateTime<Utc>>,
    pub created_at: DateTime<Utc>,
    pub started_at: Option<DateTime<Utc>>,
    pub finished_at: Option<DateTime<Utc>>,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
pub struct SessionRunPage {
    pub items: Vec<SessionRunSummary>,
    pub next_cursor: Option<String>,
}

#[derive(Clone, Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SessionRunListQuery {
    pub status: Option<RunStatus>,
    pub limit: Option<u32>,
    pub cursor: Option<String>,
}
