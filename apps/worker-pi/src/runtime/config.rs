use execution_contracts::{MachineId, PathSpec, Validate, ValidationError, WorkspaceRootId};
use llm_contracts::JsonObject;
use serde::Deserialize;
use uuid::Uuid;

#[derive(Clone, Debug, Deserialize, Eq, PartialEq)]
pub struct PiExecutionTarget {
    #[serde(alias = "machineId")]
    pub machine_id: MachineId,
    #[serde(alias = "workspaceRootId")]
    pub workspace_root_id: WorkspaceRootId,
    pub cwd: String,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq)]
pub struct PiHarnessConfig {
    pub provider: String,
    #[serde(alias = "modelId")]
    pub model_id: String,
    #[serde(alias = "reasoningLevel")]
    pub reasoning_level: String,
    #[serde(alias = "executionTarget")]
    pub execution: PiExecutionTarget,
    #[serde(default, alias = "accountId")]
    pub account_id: Option<Uuid>,
    #[serde(default, alias = "externalPrompt")]
    pub external_prompt: Option<String>,
    #[serde(default, alias = "isReplaced")]
    pub is_replaced: bool,
}

impl PiHarnessConfig {
    pub fn from_resolved(config: &JsonObject) -> Result<Self, PiHarnessConfigError> {
        let config: Self = serde_json::from_value(serde_json::Value::Object(config.clone()))
            .map_err(PiHarnessConfigError::Invalid)?;
        PathSpec::workspace(
            config.execution.workspace_root_id.clone(),
            &config.execution.cwd,
        )
        .validate()
        .map_err(PiHarnessConfigError::InvalidExecutionTarget)?;
        Ok(config)
    }
}

#[derive(Debug, thiserror::Error)]
pub enum PiHarnessConfigError {
    #[error("resolved Pi harness configuration is invalid")]
    Invalid(#[source] serde_json::Error),
    #[error("resolved Pi execution target is invalid")]
    InvalidExecutionTarget(#[source] ValidationError),
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::PiHarnessConfig;

    #[test]
    fn parses_snake_case_configuration() {
        let config = object(json!({
            "provider": "openai",
            "model_id": "gpt-5.6-sol",
            "reasoning_level": "high",
            "execution": {
                "machine_id": "machine-a",
                "workspace_root_id": "root",
                "cwd": "project"
            }
        }));

        let config = PiHarnessConfig::from_resolved(&config).expect("valid config");

        assert_eq!(config.provider, "openai");
        assert_eq!(config.model_id, "gpt-5.6-sol");
        assert_eq!(config.reasoning_level, "high");
        assert_eq!(config.execution.machine_id.as_str(), "machine-a");
        assert_eq!(config.execution.workspace_root_id.as_str(), "root");
        assert_eq!(config.execution.cwd, "project");
        assert!(!config.is_replaced);
    }

    #[test]
    fn accepts_external_camel_case_contracts() {
        let account_id = uuid::Uuid::now_v7();
        let config = object(json!({
            "provider": "openai",
            "modelId": "gpt-5.6-terra",
            "reasoningLevel": "xhigh",
            "executionTarget": {
                "machineId": "machine-a",
                "workspaceRootId": "root",
                "cwd": "project"
            },
            "accountId": account_id,
            "externalPrompt": "Follow the project rules.",
            "isReplaced": true,
            "future_setting": true
        }));

        let config = PiHarnessConfig::from_resolved(&config).expect("valid config");

        assert_eq!(config.account_id, Some(account_id));
        assert_eq!(config.execution.machine_id.as_str(), "machine-a");
        assert_eq!(config.execution.workspace_root_id.as_str(), "root");
        assert_eq!(config.execution.cwd, "project");
        assert_eq!(
            config.external_prompt.as_deref(),
            Some("Follow the project rules.")
        );
        assert!(config.is_replaced);
    }

    #[test]
    fn rejects_a_cwd_outside_the_selected_workspace_root() {
        let config = object(json!({
            "provider": "openai",
            "model_id": "gpt-5.6-sol",
            "reasoning_level": "high",
            "execution": {
                "machine_id": "machine-a",
                "workspace_root_id": "root",
                "cwd": "/outside"
            }
        }));

        assert!(PiHarnessConfig::from_resolved(&config).is_err());
    }

    fn object(value: serde_json::Value) -> llm_contracts::JsonObject {
        value.as_object().expect("object").clone()
    }
}
