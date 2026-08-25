use llm_contracts::{JsonObject, Validate, ValidationError};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use super::{finish, issue, validate_trimmed};
use crate::execution::Run;

pub use agent_contracts::{
    AbortDirective, AbortFinalizationReason, AbortResume, AcknowledgeRunAbort, RunAbort,
    RunAbortAcknowledged, RunAbortStatus, RunResume, WorkerDirective,
};

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
        let mut issues = Vec::new();
        if self.expected_state_version == 0 {
            issue(
                &mut issues,
                "expected_state_version",
                "must be greater than zero",
            );
        }
        if let Some(reason) = &self.reason {
            validate_trimmed(&mut issues, "reason", reason, 1, 2_000);
        }
        finish(issues)
    }
}

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
pub struct RunAbortResult {
    pub abort: RunAbort,
    pub run: Run,
}

#[derive(Clone, Debug, Default, Deserialize, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ResumeRun {
    pub expected_state_version: u64,
    #[serde(default)]
    pub resolution: JsonObject,
}

impl Validate for ResumeRun {
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
pub struct RunResumed {
    pub abort: RunAbort,
    pub run: Run,
}

#[derive(Clone, Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AbortListQuery {
    pub status: Option<RunAbortStatus>,
    pub limit: Option<u32>,
    pub cursor: Option<String>,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
pub struct RunAbortPage {
    pub items: Vec<RunAbort>,
    pub next_cursor: Option<String>,
}
