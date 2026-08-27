use chrono::{DateTime, Utc};
use llm_contracts::Message;
use serde::{Deserialize, Serialize};
use uuid::Uuid;

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum SessionMessageOrigin {
    External,
    Harness,
    System,
    Imported,
}

impl SessionMessageOrigin {
    #[must_use]
    pub fn from_db(value: &str) -> Option<Self> {
        match value {
            "external" => Some(Self::External),
            "harness" => Some(Self::Harness),
            "system" => Some(Self::System),
            "imported" => Some(Self::Imported),
            _ => None,
        }
    }
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum SessionMessageDelivery {
    Immediate,
    NextTurn,
}

impl SessionMessageDelivery {
    #[must_use]
    pub fn from_db(value: &str) -> Option<Self> {
        match value {
            "immediate" => Some(Self::Immediate),
            "next_turn" => Some(Self::NextTurn),
            _ => None,
        }
    }
}

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
pub struct SessionMessage {
    pub session_message_id: Uuid,
    pub session_id: Uuid,
    pub revision: u64,
    pub message: Message,
    pub origin: SessionMessageOrigin,
    pub delivery: SessionMessageDelivery,
    pub run_id: Option<Uuid>,
    pub turn_number: Option<u32>,
    pub created_at: DateTime<Utc>,
    pub committed_at: DateTime<Utc>,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
pub struct SessionMessagePage {
    pub items: Vec<SessionMessage>,
    pub next_after_revision: Option<u64>,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum RunStatus {
    Active,
    Waiting,
    Aborted,
    Completed,
    Failed,
}

impl RunStatus {
    #[must_use]
    pub const fn as_db(self) -> &'static str {
        match self {
            Self::Active => "active",
            Self::Waiting => "waiting",
            Self::Aborted => "aborted",
            Self::Completed => "completed",
            Self::Failed => "failed",
        }
    }

    #[must_use]
    pub fn from_db(value: &str) -> Option<Self> {
        match value {
            "active" => Some(Self::Active),
            "waiting" => Some(Self::Waiting),
            "aborted" => Some(Self::Aborted),
            "completed" => Some(Self::Completed),
            "failed" => Some(Self::Failed),
            _ => None,
        }
    }
}
