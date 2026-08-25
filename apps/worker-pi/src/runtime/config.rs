use execution_contracts::{MachineId, WorkspaceRootId};
use llm_contracts::JsonObject;
use serde::Deserialize;
use uuid::Uuid;

#[derive(Clone, Debug, Deserialize, Eq, PartialEq)]
pub struct PiHarnessConfig {
    pub provider: String,
    #[serde(alias = "modelId")]
    pub model_id: String,
    #[serde(alias = "reasoningLevel")]
    pub reasoning_level: String,
    #[serde(alias = "machineId")]
    pub machine_id: MachineId,
    #[serde(default, alias = "accountId")]
    pub account_id: Option<Uuid>,
    #[serde(
        default,
        alias = "workspaceRootId",
        alias = "root_id",
        alias = "rootId"
    )]
    pub workspace_root_id: Option<WorkspaceRootId>,
    #[serde(default = "default_cwd")]
    pub cwd: String,
    #[serde(default, alias = "externalPrompt")]
    pub external_prompt: Option<String>,
    #[serde(default, alias = "isReplaced")]
    pub is_replaced: bool,
}

impl PiHarnessConfig {
    pub fn from_resolved(config: &JsonObject) -> Result<Self, PiHarnessConfigError> {
        serde_json::from_value(serde_json::Value::Object(config.clone()))
            .map_err(PiHarnessConfigError::Invalid)
    }
}

#[derive(Debug, thiserror::Error)]
pub enum PiHarnessConfigError {
    #[error("resolved Pi harness configuration is invalid")]
    Invalid(#[source] serde_json::Error),
}

fn default_cwd() -> String {
    ".".to_owned()
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::PiHarnessConfig;

    #[test]
    fn parses_snake_case_configuration_and_defaults_the_cwd() {
        let config = object(json!({
            "provider": "openai",
            "model_id": "gpt-5.6-sol",
            "reasoning_level": "high",
            "machine_id": "machine-a"
        }));

        let config = PiHarnessConfig::from_resolved(&config).expect("valid config");

        assert_eq!(config.provider, "openai");
        assert_eq!(config.model_id, "gpt-5.6-sol");
        assert_eq!(config.reasoning_level, "high");
        assert_eq!(config.machine_id.as_str(), "machine-a");
        assert_eq!(config.cwd, ".");
        assert!(!config.is_replaced);
    }

    #[test]
    fn accepts_external_camel_case_contracts() {
        let account_id = uuid::Uuid::now_v7();
        let config = object(json!({
            "provider": "openai",
            "modelId": "gpt-5.6-terra",
            "reasoningLevel": "xhigh",
            "machineId": "machine-a",
            "accountId": account_id,
            "workspaceRootId": "root-a",
            "cwd": "project",
            "externalPrompt": "Follow the project rules.",
            "isReplaced": true,
            "future_setting": true
        }));

        let config = PiHarnessConfig::from_resolved(&config).expect("valid config");

        assert_eq!(config.account_id, Some(account_id));
        assert_eq!(
            config.workspace_root_id.as_ref().map(|id| id.as_str()),
            Some("root-a")
        );
        assert_eq!(config.cwd, "project");
        assert_eq!(
            config.external_prompt.as_deref(),
            Some("Follow the project rules.")
        );
        assert!(config.is_replaced);
    }

    fn object(value: serde_json::Value) -> llm_contracts::JsonObject {
        value.as_object().expect("object").clone()
    }
}
