use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::providers::model::ProviderKind;

#[derive(Debug, Deserialize)]
pub(crate) struct AgentHarness {
    pub harness_id: String,
    pub display_name: String,
    pub description: Option<String>,
    #[serde(default)]
    pub supported_providers: Vec<AgentHarnessProvider>,
    #[serde(default)]
    pub supported_reasoning_levels: Vec<String>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Deserialize)]
pub(crate) struct AgentHarnessProvider {
    pub provider_id: String,
    pub model_ids: Vec<String>,
}

#[derive(Debug, Deserialize)]
pub(crate) struct AgentHarnessPage {
    pub items: Vec<AgentHarness>,
    pub next_cursor: Option<String>,
}

#[derive(Debug, Serialize)]
pub struct HarnessSummary {
    pub id: String,
    pub name: String,
    pub description: Option<String>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

impl From<AgentHarness> for HarnessSummary {
    fn from(harness: AgentHarness) -> Self {
        Self {
            id: harness.harness_id,
            name: harness.display_name,
            description: harness.description,
            created_at: harness.created_at,
            updated_at: harness.updated_at,
        }
    }
}

#[derive(Debug, Eq, PartialEq, Serialize)]
pub struct HarnessModelOptions {
    pub providers: Vec<HarnessProviderModelOptions>,
    pub reasoning_levels: Vec<String>,
}

#[derive(Debug, Eq, PartialEq, Serialize)]
pub struct HarnessProviderModelOptions {
    pub account_id: Uuid,
    pub name: String,
    pub provider: ProviderKind,
    pub model_ids: Vec<String>,
}
