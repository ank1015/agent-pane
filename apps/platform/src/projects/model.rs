use agent_contracts::{Run, SessionMessage};
use execution_protocol::ProjectEnvironment;
use llm_contracts::{ImageContent, JsonObject};
use serde::{Deserialize, Deserializer, Serialize};
use uuid::Uuid;

use crate::providers::model::{Provider, ProviderKind};
use crate::upstream::agent::AgentSession;

#[derive(Debug, Serialize, sqlx::FromRow)]
pub struct Project {
    pub id: Uuid,
    pub name: String,
    pub avatar: Option<String>,
}

#[derive(Debug, Serialize)]
pub struct ProjectBootstrap {
    pub harnesses: Vec<ProjectBootstrapHarness>,
    pub provider_accounts: Vec<ProjectBootstrapProviderAccount>,
    pub project_environments: Vec<ProjectEnvironment>,
}

#[derive(Debug, Serialize)]
pub struct ProjectBootstrapHarness {
    pub harness_id: String,
    pub active_revision_id: String,
    pub config_schema: Option<JsonObject>,
    pub supported_providers: Vec<ProjectBootstrapHarnessProvider>,
    pub supported_reasoning_levels: Vec<String>,
}

#[derive(Debug, Serialize)]
pub struct ProjectBootstrapHarnessProvider {
    pub provider_id: String,
    pub model_ids: Vec<String>,
}

#[derive(Debug, Eq, PartialEq, Serialize)]
pub struct ProjectBootstrapProviderAccount {
    pub account_id: Uuid,
    pub name: String,
    pub provider: ProviderKind,
}

impl From<Provider> for ProjectBootstrapProviderAccount {
    fn from(account: Provider) -> Self {
        Self {
            account_id: account.id,
            name: account.name,
            provider: account.provider,
        }
    }
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CreateProjectRequest {
    pub name: String,
    #[serde(default)]
    pub avatar: Option<String>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct UpdateProjectRequest {
    #[serde(default)]
    pub name: Option<String>,
    #[serde(default, deserialize_with = "deserialize_present_nullable")]
    pub avatar: Option<Option<String>>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct CreateProjectHarnessSessionRequest {
    pub harness_id: String,
    pub prompt: String,
    #[serde(default, alias = "attachements")]
    pub attachments: Vec<ImageContent>,
    #[serde(default)]
    pub config_override: JsonObject,
    #[serde(default)]
    pub limits: ProjectHarnessRunLimits,
}

#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ProjectHarnessRunLimits {
    pub max_turns: Option<u32>,
}

#[derive(Debug, Serialize)]
pub struct CreateProjectHarnessSessionResponse {
    pub session: AgentSession,
    pub trigger_message: SessionMessage,
    pub run: Run,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct StartProjectHarnessSessionRunRequest {
    pub prompt: String,
    #[serde(default, alias = "attachements")]
    pub attachments: Vec<ImageContent>,
    #[serde(default)]
    pub config_override: JsonObject,
    #[serde(default)]
    pub limits: ProjectHarnessRunLimits,
    pub expected_session_revision: u64,
}

#[derive(Debug, Serialize)]
pub struct StartProjectHarnessSessionRunResponse {
    pub trigger_message: SessionMessage,
    pub run: Run,
}

#[derive(Debug, Serialize)]
pub struct ProjectHarnessSessionResponse {
    pub session: AgentSession,
    pub harness_id: String,
    pub latest_run: Option<Run>,
    pub active_run: Option<Run>,
}

fn deserialize_present_nullable<'de, D>(deserializer: D) -> Result<Option<Option<String>>, D::Error>
where
    D: Deserializer<'de>,
{
    Option::<String>::deserialize(deserializer).map(Some)
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::UpdateProjectRequest;

    #[test]
    fn update_distinguishes_omitted_and_null_avatar() {
        let omitted: UpdateProjectRequest =
            serde_json::from_value(json!({ "name": "Renamed" })).unwrap();
        let cleared: UpdateProjectRequest =
            serde_json::from_value(json!({ "avatar": null })).unwrap();
        let replaced: UpdateProjectRequest =
            serde_json::from_value(json!({ "avatar": "https://example.com/avatar.png" })).unwrap();

        assert!(omitted.avatar.is_none());
        assert_eq!(cleared.avatar, Some(None));
        assert_eq!(
            replaced.avatar,
            Some(Some("https://example.com/avatar.png".to_owned()))
        );
    }
}
