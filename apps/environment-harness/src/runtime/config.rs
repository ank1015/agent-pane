use llm_contracts::JsonObject;
use serde::Deserialize;
use uuid::Uuid;

#[derive(Clone, Debug, Deserialize, Eq, PartialEq)]
pub struct EnvironmentHarnessConfig {
    pub provider: String,
    #[serde(alias = "modelId")]
    pub model_id: String,
    #[serde(alias = "reasoningLevel")]
    pub reasoning_level: String,
    #[serde(default, alias = "accountId")]
    pub account_id: Option<Uuid>,
    #[serde(alias = "projectId")]
    pub project_id: Uuid,
}

impl EnvironmentHarnessConfig {
    pub fn from_resolved(config: &JsonObject) -> Result<Self, EnvironmentHarnessConfigError> {
        serde_json::from_value(serde_json::Value::Object(config.clone()))
            .map_err(EnvironmentHarnessConfigError::Invalid)
    }
}

#[derive(Debug, thiserror::Error)]
pub enum EnvironmentHarnessConfigError {
    #[error("resolved environment harness configuration is invalid")]
    Invalid(#[source] serde_json::Error),
}

#[cfg(test)]
mod tests {
    use serde_json::json;
    use uuid::Uuid;

    use super::EnvironmentHarnessConfig;

    #[test]
    fn parses_snake_case_configuration() {
        let config = object(json!({
            "provider": "openai",
            "model_id": "gpt-5.6-sol",
            "reasoning_level": "high",
            "project_id": "019d2aa0-0000-7000-8000-000000000010",
        }));

        let config = EnvironmentHarnessConfig::from_resolved(&config).expect("valid config");

        assert_eq!(config.provider, "openai");
        assert_eq!(config.model_id, "gpt-5.6-sol");
        assert_eq!(config.reasoning_level, "high");
        assert_eq!(
            config.project_id,
            "019d2aa0-0000-7000-8000-000000000010"
                .parse::<Uuid>()
                .unwrap()
        );
    }

    #[test]
    fn accepts_external_camel_case_contracts() {
        let account_id = uuid::Uuid::now_v7();
        let project_id = uuid::Uuid::now_v7();
        let config = object(json!({
            "provider": "openai",
            "modelId": "gpt-5.6-terra",
            "reasoningLevel": "xhigh",
            "accountId": account_id,
            "projectId": project_id,
            "future_setting": true
        }));

        let config = EnvironmentHarnessConfig::from_resolved(&config).expect("valid config");

        assert_eq!(config.account_id, Some(account_id));
        assert_eq!(config.project_id, project_id);
    }

    #[test]
    fn rejects_configuration_without_project_context() {
        let config = object(json!({
            "provider": "openai",
            "model_id": "gpt-5.6-sol",
            "reasoning_level": "high"
        }));

        assert!(EnvironmentHarnessConfig::from_resolved(&config).is_err());
    }

    fn object(value: serde_json::Value) -> llm_contracts::JsonObject {
        value.as_object().expect("object").clone()
    }
}
