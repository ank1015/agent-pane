use chrono::{DateTime, Utc};
use llm_contracts::{JsonObject, Validate, ValidationError};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::validation::{finish, issue, validate_trimmed};

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

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum WaitResolutionSource {
    External,
    Expired,
}

impl WaitResolutionSource {
    #[must_use]
    pub const fn as_db(self) -> &'static str {
        match self {
            Self::External => "external",
            Self::Expired => "expired",
        }
    }

    #[must_use]
    pub fn from_db(value: &str) -> Option<Self> {
        match value {
            "external" => Some(Self::External),
            "expired" => Some(Self::Expired),
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
    pub expires_at: Option<DateTime<Utc>>,
    pub status: WaitStatus,
    pub resolution_source: Option<WaitResolutionSource>,
    pub resolution: Option<JsonObject>,
    pub requested_at: DateTime<Utc>,
    pub resolved_at: Option<DateTime<Utc>>,
    pub cancelled_at: Option<DateTime<Utc>>,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
pub struct WaitResume {
    pub wait_id: Uuid,
    pub harness_wait_id: String,
    pub kind: String,
    pub public_request: JsonObject,
    pub resume_metadata: JsonObject,
    pub source: WaitResolutionSource,
    pub resolution: JsonObject,
    pub resolved_at: DateTime<Utc>,
}

impl Validate for WaitResume {
    fn validate(&self) -> Result<(), ValidationError> {
        let mut issues = Vec::new();
        if self.wait_id.is_nil() {
            issue(&mut issues, "wait_id", "must not be nil");
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
