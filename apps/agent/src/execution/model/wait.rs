use llm_contracts::{JsonObject, Validate, ValidationError};
use serde::{Deserialize, Serialize};

use crate::execution::Run;

pub use agent_contracts::{RequestRunWait, RunWait, RunWaitRequested, WaitResume, WaitStatus};

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
