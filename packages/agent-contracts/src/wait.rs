use chrono::{DateTime, Utc};
use llm_contracts::{JsonObject, Validate, ValidationError};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::{Run, validation::finish, validation::issue, validation::validate_trimmed};

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum WaitStatus {
    Pending,
    Resolved,
    Cancelled,
}

impl WaitStatus {
    #[must_use]
    pub const fn as_db(self) -> &'static str {
        match self {
            Self::Pending => "pending",
            Self::Resolved => "resolved",
            Self::Cancelled => "cancelled",
        }
    }

    #[must_use]
    pub fn from_db(value: &str) -> Option<Self> {
        match value {
            "pending" => Some(Self::Pending),
            "resolved" => Some(Self::Resolved),
            "cancelled" => Some(Self::Cancelled),
            _ => None,
        }
    }
}

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
pub struct RunWait {
    pub wait_id: Uuid,
    pub run_id: Uuid,
    pub turn_number: u32,
    pub harness_wait_id: String,
    pub kind: String,
    pub public_request: JsonObject,
    pub resume_metadata: JsonObject,
    pub status: WaitStatus,
    pub resolution: Option<JsonObject>,
    pub requested_at: DateTime<Utc>,
    pub resolved_at: Option<DateTime<Utc>>,
    pub cancelled_at: Option<DateTime<Utc>>,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct RequestRunWait {
    pub lease_version: u64,
    pub expected_state_version: u64,
    pub wait_id: Uuid,
    pub harness_wait_id: String,
    pub kind: String,
    #[serde(default)]
    pub public_request: JsonObject,
    #[serde(default)]
    pub resume_metadata: JsonObject,
}

impl Validate for RequestRunWait {
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
        validate_trimmed(
            &mut issues,
            "harness_wait_id",
            &self.harness_wait_id,
            1,
            256,
        );
        validate_trimmed(&mut issues, "kind", &self.kind, 1, 128);
        finish(issues)
    }
}

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
pub struct RunWaitRequested {
    pub wait: RunWait,
    pub run: Run,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
pub struct WaitResume {
    pub wait_id: Uuid,
    pub harness_wait_id: String,
    pub kind: String,
    pub public_request: JsonObject,
    pub resume_metadata: JsonObject,
    pub resolution: JsonObject,
    pub resolved_at: DateTime<Utc>,
}
