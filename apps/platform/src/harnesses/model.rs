use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

#[derive(Debug, Deserialize)]
pub(crate) struct AgentHarness {
    pub harness_id: String,
    pub display_name: String,
    pub description: Option<String>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
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
