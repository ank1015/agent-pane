use chrono::{DateTime, Utc};
use llm_contracts::JsonObject;
use serde::{Deserialize, Serialize};

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum HarnessRevisionStatus {
    Registered,
    Active,
    Deprecated,
    Retired,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
pub struct HarnessRevision {
    pub harness_revision_id: String,
    pub harness_id: String,
    pub revision: String,
    pub contract_version: u32,
    pub status: HarnessRevisionStatus,
    pub default_config: JsonObject,
    pub config_schema: Option<JsonObject>,
    pub first_activated_at: Option<DateTime<Utc>>,
    pub retired_at: Option<DateTime<Utc>>,
    pub created_at: DateTime<Utc>,
}
