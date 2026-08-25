use chrono::{DateTime, Utc};
use llm_contracts::{JsonObject, Validate, ValidationError};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use uuid::Uuid;

use crate::{SessionMessage, validation::finish, validation::issue};

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
pub struct Run {
    pub run_id: Uuid,
    pub session_id: Uuid,
    pub trigger_message_id: Uuid,
    pub harness_revision_id: String,
    pub resolved_config: JsonObject,
    pub status: crate::RunStatus,
    pub current_turn: u32,
    pub max_turns: u32,
    pub failures_in_current_turn: u32,
    pub max_failures_per_turn: u32,
    pub state_version: u64,
    pub queued_at: Option<DateTime<Utc>>,
    pub final_message_id: Option<Uuid>,
    pub failure: Option<Value>,
    pub created_at: DateTime<Utc>,
    pub started_at: Option<DateTime<Utc>>,
    pub finished_at: Option<DateTime<Utc>>,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum TurnCompletionDisposition {
    Continue,
    Complete,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct CompleteRunTurn {
    pub lease_version: u64,
    pub expected_state_version: u64,
    pub disposition: TurnCompletionDisposition,
    pub final_message_id: Option<Uuid>,
}

impl Validate for CompleteRunTurn {
    fn validate(&self) -> Result<(), ValidationError> {
        let mut issues = Vec::new();
        for (path, value) in [
            ("lease_version", self.lease_version),
            ("expected_state_version", self.expected_state_version),
        ] {
            if value == 0 {
                issue(&mut issues, path, "must be greater than zero");
            }
        }
        match (self.disposition, self.final_message_id) {
            (TurnCompletionDisposition::Continue, Some(_)) => issue(
                &mut issues,
                "final_message_id",
                "must be absent when disposition is continue",
            ),
            (TurnCompletionDisposition::Complete, None) => issue(
                &mut issues,
                "final_message_id",
                "is required when disposition is complete",
            ),
            _ => {}
        }
        finish(issues)
    }
}

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
pub struct RunTurnCompleted {
    pub run: Run,
    pub committed_messages: Vec<SessionMessage>,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct FailRunTurn {
    pub lease_version: u64,
    pub expected_state_version: u64,
    pub failure: JsonObject,
}

impl Validate for FailRunTurn {
    fn validate(&self) -> Result<(), ValidationError> {
        let mut issues = Vec::new();
        for (path, value) in [
            ("lease_version", self.lease_version),
            ("expected_state_version", self.expected_state_version),
        ] {
            if value == 0 {
                issue(&mut issues, path, "must be greater than zero");
            }
        }
        finish(issues)
    }
}

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
pub struct RunTurnFailed {
    pub run: Run,
    pub will_retry: bool,
}
