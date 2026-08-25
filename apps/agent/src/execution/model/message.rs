use chrono::{DateTime, Utc};
use llm_contracts::{Message, Validate, ValidationError};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use super::{append_nested, finish, issue};

pub use agent_contracts::{
    AppendRunMessages, NewRunMessage, RunMessagesAppended, SessionMessage, SessionMessageDelivery,
    SessionMessageOrigin,
};

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct QueueRunMessage {
    pub expected_state_version: u64,
    pub input: NewRunMessage,
}

impl Validate for QueueRunMessage {
    fn validate(&self) -> Result<(), ValidationError> {
        let mut issues = Vec::new();
        if self.expected_state_version == 0 {
            issue(
                &mut issues,
                "expected_state_version",
                "must be greater than zero",
            );
        }
        if let Err(error) = self.input.validate() {
            append_nested(&mut issues, "input", error);
        }
        finish(issues)
    }
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum QueuedRunMessageState {
    Pending,
    Committed,
    Discarded,
}

impl QueuedRunMessageState {
    pub(crate) const fn as_db(self) -> &'static str {
        match self {
            Self::Pending => "pending",
            Self::Committed => "committed",
            Self::Discarded => "discarded",
        }
    }
}

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
pub struct QueuedRunMessage {
    pub session_message_id: Uuid,
    pub session_id: Uuid,
    pub message: Message,
    pub state: QueuedRunMessageState,
    pub revision: Option<u64>,
    pub run_id: Uuid,
    pub queued_during_turn: u32,
    pub queue_sequence: u64,
    pub discard_reason: Option<String>,
    pub created_at: DateTime<Utc>,
    pub committed_at: Option<DateTime<Utc>>,
    pub discarded_at: Option<DateTime<Utc>>,
}

#[derive(Clone, Copy, Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct QueuedRunMessageListQuery {
    pub state: Option<QueuedRunMessageState>,
    pub after_sequence: Option<u64>,
    pub limit: Option<u32>,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
pub struct QueuedRunMessagePage {
    pub items: Vec<QueuedRunMessage>,
    pub next_after_sequence: Option<u64>,
}
