use chrono::{DateTime, Utc};
use llm_contracts::{JsonObject, Message, Validate, ValidationError};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use super::ExecutionError;

pub use agent_contracts::{
    AppendSessionMessages, NewRunMessage, Run, RunAbort, RunStatus, RunWait, SessionMessage,
    SessionMessageDelivery, SessionMessageOrigin, SessionMessagesAppended, WaitResolutionSource,
    WaitStatus,
};

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(tag = "selection", rename_all = "snake_case")]
pub enum HarnessSelection {
    ActiveRevision { harness_id: String },
    ExactRevision { harness_revision_id: String },
}

impl Validate for HarnessSelection {
    fn validate(&self) -> Result<(), ValidationError> {
        let value = match self {
            Self::ActiveRevision { harness_id } => harness_id,
            Self::ExactRevision {
                harness_revision_id,
            } => harness_revision_id,
        };
        if value.is_empty() || value != value.trim() {
            return Err(ValidationError::single(
                "harness",
                "identifier must be non-empty and trimmed",
            ));
        }
        Ok(())
    }
}

#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct RunLimits {
    pub max_turns: Option<u32>,
}

impl Validate for RunLimits {
    fn validate(&self) -> Result<(), ValidationError> {
        if self.max_turns == Some(0) {
            Err(ValidationError::single(
                "max_turns",
                "must be greater than zero",
            ))
        } else {
            Ok(())
        }
    }
}

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct StartRun {
    pub run_id: Uuid,
    pub input: NewRunMessage,
    pub harness: HarnessSelection,
    #[serde(default)]
    pub config_override: JsonObject,
    #[serde(default)]
    pub limits: RunLimits,
    pub expected_session_revision: Option<u64>,
}

impl Validate for StartRun {
    fn validate(&self) -> Result<(), ValidationError> {
        self.input.validate()?;
        self.harness.validate()?;
        self.limits.validate()
    }
}

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
pub struct RunAccepted {
    pub trigger_message: SessionMessage,
    pub run: Run,
}

pub struct CreateOutcome<T> {
    pub value: T,
    pub created: bool,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ExecutionPolicy {
    default_max_turns: u32,
    max_turns: u32,
    max_message_batch: usize,
}

impl ExecutionPolicy {
    pub fn new(
        default_max_turns: u32,
        max_turns: u32,
        max_message_batch: usize,
    ) -> Result<Self, ExecutionPolicyError> {
        if default_max_turns == 0 {
            return Err(ExecutionPolicyError::NotPositive("default_max_turns"));
        }
        if max_turns == 0 {
            return Err(ExecutionPolicyError::NotPositive("max_turns"));
        }
        if max_message_batch == 0 {
            return Err(ExecutionPolicyError::NotPositive("max_message_batch"));
        }
        if default_max_turns > max_turns {
            return Err(ExecutionPolicyError::DefaultExceedsMaximum);
        }
        Ok(Self {
            default_max_turns,
            max_turns,
            max_message_batch,
        })
    }

    pub(super) fn resolve(self, requested: RunLimits) -> Result<u32, ExecutionError> {
        let value = requested.max_turns.unwrap_or(self.default_max_turns);
        if value > self.max_turns {
            return Err(ExecutionError::RunLimitExceeded {
                requested: value,
                maximum: self.max_turns,
            });
        }
        Ok(value)
    }

    pub(super) const fn max_message_batch(self) -> usize {
        self.max_message_batch
    }
}

impl Default for ExecutionPolicy {
    fn default() -> Self {
        Self {
            default_max_turns: 100,
            max_turns: 1_000,
            max_message_batch: 100,
        }
    }
}

#[derive(Debug, thiserror::Error)]
pub enum ExecutionPolicyError {
    #[error("{0} must be greater than zero")]
    NotPositive(&'static str),
    #[error("default_max_turns cannot exceed max_turns")]
    DefaultExceedsMaximum,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct QueueRunMessage {
    pub expected_state_version: u64,
    pub input: NewRunMessage,
}

impl Validate for QueueRunMessage {
    fn validate(&self) -> Result<(), ValidationError> {
        if self.expected_state_version == 0 {
            return Err(ValidationError::single(
                "expected_state_version",
                "must be greater than zero",
            ));
        }
        self.input.validate()
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
    pub(super) const fn as_db(self) -> &'static str {
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

#[derive(Clone, Debug, Default, Deserialize, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ResolveRunWait {
    pub expected_state_version: u64,
    #[serde(default)]
    pub resolution: JsonObject,
}

impl Validate for ResolveRunWait {
    fn validate(&self) -> Result<(), ValidationError> {
        if self.expected_state_version == 0 {
            Err(ValidationError::single(
                "expected_state_version",
                "must be greater than zero",
            ))
        } else {
            Ok(())
        }
    }
}

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
pub struct RunWaitResolved {
    pub wait: RunWait,
    pub run: Run,
}

#[derive(Clone, Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct WaitListQuery {
    pub status: Option<WaitStatus>,
    pub limit: Option<u32>,
    pub cursor: Option<String>,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
pub struct RunWaitPage {
    pub items: Vec<RunWait>,
    pub next_cursor: Option<String>,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct RequestRunAbort {
    pub abort_id: Uuid,
    pub expected_state_version: u64,
    pub reason: Option<String>,
    #[serde(default)]
    pub payload: JsonObject,
}

impl Validate for RequestRunAbort {
    fn validate(&self) -> Result<(), ValidationError> {
        if self.abort_id.is_nil() {
            return Err(ValidationError::single("abort_id", "must not be nil"));
        }
        if self.expected_state_version == 0 {
            return Err(ValidationError::single(
                "expected_state_version",
                "must be greater than zero",
            ));
        }
        if let Some(reason) = &self.reason
            && (reason.is_empty() || reason != reason.trim() || reason.chars().count() > 2_000)
        {
            return Err(ValidationError::single(
                "reason",
                "must be trimmed and contain 1 to 2000 characters",
            ));
        }
        Ok(())
    }
}

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
pub struct RunAbortResult {
    pub abort: RunAbort,
    pub run: Run,
}

#[derive(Clone, Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AbortListQuery {
    pub limit: Option<u32>,
    pub cursor: Option<String>,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
pub struct RunAbortPage {
    pub items: Vec<RunAbort>,
    pub next_cursor: Option<String>,
}

#[derive(Clone, Copy, Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct HarnessMessageListQuery {
    pub after_revision: Option<u64>,
    pub limit: Option<u32>,
}
