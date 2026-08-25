use chrono::{DateTime, Utc};
use llm_contracts::{JsonObject, Validate, ValidationError};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::{Run, WaitResume, validation::finish, validation::issue};

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum RunAbortStatus {
    Pending,
    Finalized,
    Resumed,
}

impl RunAbortStatus {
    #[must_use]
    pub const fn as_db(self) -> &'static str {
        match self {
            Self::Pending => "pending",
            Self::Finalized => "finalized",
            Self::Resumed => "resumed",
        }
    }

    #[must_use]
    pub fn from_db(value: &str) -> Option<Self> {
        match value {
            "pending" => Some(Self::Pending),
            "finalized" => Some(Self::Finalized),
            "resumed" => Some(Self::Resumed),
            _ => None,
        }
    }
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum AbortFinalizationReason {
    Acknowledged,
    NoWorker,
    LeaseExpired,
    DeadlineExpired,
}

impl AbortFinalizationReason {
    #[must_use]
    pub const fn as_db(self) -> &'static str {
        match self {
            Self::Acknowledged => "acknowledged",
            Self::NoWorker => "no_worker",
            Self::LeaseExpired => "lease_expired",
            Self::DeadlineExpired => "deadline_expired",
        }
    }

    #[must_use]
    pub fn from_db(value: &str) -> Option<Self> {
        match value {
            "acknowledged" => Some(Self::Acknowledged),
            "no_worker" => Some(Self::NoWorker),
            "lease_expired" => Some(Self::LeaseExpired),
            "deadline_expired" => Some(Self::DeadlineExpired),
            _ => None,
        }
    }
}

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
pub struct RunAbort {
    pub abort_id: Uuid,
    pub run_id: Uuid,
    pub turn_number: u32,
    pub sequence: u64,
    pub reason: Option<String>,
    pub payload: JsonObject,
    pub status: RunAbortStatus,
    pub requested_at: DateTime<Utc>,
    pub delivered_at: Option<DateTime<Utc>>,
    pub deadline_at: DateTime<Utc>,
    pub finalized_at: Option<DateTime<Utc>>,
    pub finalization_reason: Option<AbortFinalizationReason>,
    pub resume_metadata: JsonObject,
    pub resolution: Option<JsonObject>,
    pub resumed_at: Option<DateTime<Utc>>,
    pub resumed_turn_number: Option<u32>,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct AbortDirective {
    pub abort_id: Uuid,
    pub reason: Option<String>,
    pub payload: JsonObject,
    pub requested_at: DateTime<Utc>,
    pub deadline_at: DateTime<Utc>,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(tag = "type", content = "details", rename_all = "snake_case")]
pub enum WorkerDirective {
    Abort(AbortDirective),
}

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct AcknowledgeRunAbort {
    pub abort_id: Uuid,
    pub lease_version: u64,
    pub expected_state_version: u64,
    #[serde(default)]
    pub resume_metadata: JsonObject,
}

impl Validate for AcknowledgeRunAbort {
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
pub struct RunAbortAcknowledged {
    pub abort: RunAbort,
    pub run: Run,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
pub struct AbortResume {
    pub abort_id: Uuid,
    pub reason: Option<String>,
    pub payload: JsonObject,
    pub resume_metadata: JsonObject,
    pub resolution: JsonObject,
    pub finalized_at: DateTime<Utc>,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
#[serde(tag = "source", content = "details", rename_all = "snake_case")]
pub enum RunResume {
    Wait(WaitResume),
    Abort(AbortResume),
}
