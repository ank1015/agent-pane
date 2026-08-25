use llm_contracts::{JsonObject, Validate, ValidationError};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use super::{NewRunMessage, append_nested, finish, issue, validate_id};
pub use crate::sessions::RunStatus;
pub use agent_contracts::{
    CompleteRunTurn, FailRunTurn, Run, RunTurnCompleted, RunTurnFailed, TurnCompletionDisposition,
};

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(tag = "selection", rename_all = "snake_case")]
pub enum HarnessSelection {
    ActiveRevision { harness_id: String },
    ExactRevision { harness_revision_id: String },
}

impl Validate for HarnessSelection {
    fn validate(&self) -> Result<(), ValidationError> {
        let mut issues = Vec::new();
        match self {
            Self::ActiveRevision { harness_id } => {
                validate_id(&mut issues, "harness_id", harness_id);
            }
            Self::ExactRevision {
                harness_revision_id,
            } => validate_id(&mut issues, "harness_revision_id", harness_revision_id),
        }
        finish(issues)
    }
}

#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct RunLimits {
    pub max_turns: Option<u32>,
    pub max_failures_per_turn: Option<u32>,
}

impl Validate for RunLimits {
    fn validate(&self) -> Result<(), ValidationError> {
        let mut issues = Vec::new();
        if self.max_turns == Some(0) {
            issue(&mut issues, "max_turns", "must be greater than zero");
        }
        if self.max_failures_per_turn == Some(0) {
            issue(
                &mut issues,
                "max_failures_per_turn",
                "must be greater than zero",
            );
        }
        finish(issues)
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
        let mut issues = Vec::new();
        if let Err(error) = self.input.validate() {
            append_nested(&mut issues, "input", error);
        }
        if let Err(error) = self.harness.validate() {
            append_nested(&mut issues, "harness", error);
        }
        if let Err(error) = self.limits.validate() {
            append_nested(&mut issues, "limits", error);
        }
        finish(issues)
    }
}

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
pub struct RunAccepted {
    pub trigger_message: crate::sessions::SessionMessage,
    pub run: Run,
}
