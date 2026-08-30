use std::fmt;

use execution_contracts::{MachineId, PathSpec, Validate, ValidationError, WorkspaceRootId};
use llm_contracts::JsonObject;
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use super::CodexModel;

pub const SUPPORTED_PROVIDER_IDS: &[&str] = &["openai", "chatgpt"];
pub const SUPPORTED_REASONING_LEVELS: &[&str] = &["low", "medium", "high", "xhigh", "max"];

#[derive(Clone, Copy, Debug, Deserialize, Eq, Hash, PartialEq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum CodexProvider {
    OpenAi,
    ChatGpt,
}

impl CodexProvider {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::OpenAi => "openai",
            Self::ChatGpt => "chatgpt",
        }
    }
}

impl fmt::Display for CodexProvider {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.as_str())
    }
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, Hash, PartialEq)]
#[serde(rename_all = "lowercase")]
pub enum ReasoningLevel {
    Low,
    Medium,
    High,
    Xhigh,
    Max,
}

impl ReasoningLevel {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Low => "low",
            Self::Medium => "medium",
            Self::High => "high",
            Self::Xhigh => "xhigh",
            Self::Max => "max",
        }
    }
}

impl fmt::Display for ReasoningLevel {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.as_str())
    }
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq)]
pub struct CodexExecutionTarget {
    #[serde(alias = "machineId")]
    pub machine_id: MachineId,
    #[serde(alias = "workspaceRootId")]
    pub workspace_root_id: WorkspaceRootId,
    pub cwd: String,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CodexHarnessConfig {
    pub provider: CodexProvider,
    pub model: CodexModel,
    pub reasoning_level: ReasoningLevel,
    pub execution: CodexExecutionTarget,
    pub account_id: Option<Uuid>,
    pub external_prompt: Option<String>,
    pub is_replaced: bool,
}

impl CodexHarnessConfig {
    pub fn from_resolved(config: &JsonObject) -> Result<Self, CodexHarnessConfigError> {
        let raw: RawCodexHarnessConfig =
            serde_json::from_value(serde_json::Value::Object(config.clone()))
                .map_err(CodexHarnessConfigError::Invalid)?;
        let provider = parse_provider(&raw.provider)?;
        let model = parse_model(&raw.model_id)?;
        let reasoning_level = parse_reasoning_level(&raw.reasoning_level)?;
        PathSpec::workspace(raw.execution.workspace_root_id.clone(), &raw.execution.cwd)
            .validate()
            .map_err(CodexHarnessConfigError::InvalidExecutionTarget)?;
        Ok(Self {
            provider,
            model,
            reasoning_level,
            execution: raw.execution,
            account_id: raw.account_id,
            external_prompt: raw.external_prompt,
            is_replaced: raw.is_replaced,
        })
    }
}

#[derive(Deserialize)]
struct RawCodexHarnessConfig {
    provider: String,
    #[serde(alias = "modelId")]
    model_id: String,
    #[serde(alias = "reasoningLevel")]
    reasoning_level: String,
    #[serde(alias = "executionTarget")]
    execution: CodexExecutionTarget,
    #[serde(default, alias = "accountId")]
    account_id: Option<Uuid>,
    #[serde(default, alias = "externalPrompt")]
    external_prompt: Option<String>,
    #[serde(default, alias = "isReplaced")]
    is_replaced: bool,
}

fn parse_provider(provider: &str) -> Result<CodexProvider, CodexHarnessConfigError> {
    match provider {
        "openai" => Ok(CodexProvider::OpenAi),
        "chatgpt" => Ok(CodexProvider::ChatGpt),
        _ => Err(CodexHarnessConfigError::UnsupportedProvider(
            provider.to_owned(),
        )),
    }
}

fn parse_model(model: &str) -> Result<CodexModel, CodexHarnessConfigError> {
    match model {
        "gpt-5.6-sol" => Ok(CodexModel::Sol),
        "gpt-5.6-terra" => Ok(CodexModel::Terra),
        "gpt-5.6-luna" => Ok(CodexModel::Luna),
        _ => Err(CodexHarnessConfigError::UnsupportedModel(model.to_owned())),
    }
}

fn parse_reasoning_level(reasoning_level: &str) -> Result<ReasoningLevel, CodexHarnessConfigError> {
    match reasoning_level {
        "low" => Ok(ReasoningLevel::Low),
        "medium" => Ok(ReasoningLevel::Medium),
        "high" => Ok(ReasoningLevel::High),
        "xhigh" => Ok(ReasoningLevel::Xhigh),
        "max" => Ok(ReasoningLevel::Max),
        _ => Err(CodexHarnessConfigError::UnsupportedReasoningLevel(
            reasoning_level.to_owned(),
        )),
    }
}

#[derive(Debug, thiserror::Error)]
pub enum CodexHarnessConfigError {
    #[error("resolved Codex harness configuration is invalid")]
    Invalid(#[source] serde_json::Error),
    #[error("unsupported Codex provider {0:?}")]
    UnsupportedProvider(String),
    #[error("unsupported Codex model {0:?}")]
    UnsupportedModel(String),
    #[error("unsupported Codex reasoning level {0:?}")]
    UnsupportedReasoningLevel(String),
    #[error("resolved Codex execution target is invalid")]
    InvalidExecutionTarget(#[source] ValidationError),
}

#[cfg(test)]
mod tests {
    use llm_contracts::JsonObject;
    use serde_json::json;

    use super::{
        CodexHarnessConfig, CodexHarnessConfigError, CodexModel, CodexProvider, ReasoningLevel,
    };

    #[test]
    fn parses_every_supported_model_for_both_providers() {
        for provider in ["openai", "chatgpt"] {
            for (model_id, model) in [
                ("gpt-5.6-sol", CodexModel::Sol),
                ("gpt-5.6-terra", CodexModel::Terra),
                ("gpt-5.6-luna", CodexModel::Luna),
            ] {
                let config =
                    CodexHarnessConfig::from_resolved(&resolved(provider, model_id, "high"))
                        .expect("supported config");
                assert_eq!(config.provider.as_str(), provider);
                assert_eq!(config.model, model);
                assert_eq!(config.reasoning_level, ReasoningLevel::High);
                assert!(config.model.profile().code_mode);
            }
        }
    }

    #[test]
    fn accepts_external_aliases_and_optional_fields() {
        let account_id = uuid::Uuid::now_v7();
        let config = object(json!({
            "provider": "chatgpt",
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

        let config = CodexHarnessConfig::from_resolved(&config).expect("valid config");
        assert_eq!(config.provider, CodexProvider::ChatGpt);
        assert_eq!(config.model, CodexModel::Terra);
        assert_eq!(config.reasoning_level, ReasoningLevel::Xhigh);
        assert_eq!(config.account_id, Some(account_id));
        assert_eq!(
            config.external_prompt.as_deref(),
            Some("Follow the project rules.")
        );
        assert!(config.is_replaced);
    }

    #[test]
    fn rejects_unsupported_provider_model_and_reasoning_level() {
        assert!(matches!(
            CodexHarnessConfig::from_resolved(&resolved(
                "anthropic",
                "gpt-5.6-sol",
                "high"
            )),
            Err(CodexHarnessConfigError::UnsupportedProvider(provider)) if provider == "anthropic"
        ));
        assert!(matches!(
            CodexHarnessConfig::from_resolved(&resolved("openai", "gpt-5.5", "high")),
            Err(CodexHarnessConfigError::UnsupportedModel(model)) if model == "gpt-5.5"
        ));
        assert!(matches!(
            CodexHarnessConfig::from_resolved(&resolved(
                "openai",
                "gpt-5.6-sol",
                "ultra"
            )),
            Err(CodexHarnessConfigError::UnsupportedReasoningLevel(level)) if level == "ultra"
        ));
    }

    #[test]
    fn rejects_a_cwd_outside_the_selected_workspace_root() {
        let mut config = resolved("openai", "gpt-5.6-luna", "medium");
        config.insert(
            "execution".to_owned(),
            json!({
                "machine_id": "machine-a",
                "workspace_root_id": "root",
                "cwd": "/outside"
            }),
        );
        assert!(matches!(
            CodexHarnessConfig::from_resolved(&config),
            Err(CodexHarnessConfigError::InvalidExecutionTarget(_))
        ));
    }

    fn resolved(provider: &str, model_id: &str, reasoning_level: &str) -> JsonObject {
        object(json!({
            "provider": provider,
            "model_id": model_id,
            "reasoning_level": reasoning_level,
            "execution": {
                "machine_id": "machine-a",
                "workspace_root_id": "root",
                "cwd": "project"
            }
        }))
    }

    fn object(value: serde_json::Value) -> JsonObject {
        value.as_object().expect("object").clone()
    }
}
