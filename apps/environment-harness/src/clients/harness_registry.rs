use agent_harness_sdk::{
    AgentControlServiceConfig, CreateHarnessRequest, HarnessDescriptor, HarnessProvider,
    HarnessReasoningLevel, HarnessRegistryClient, HarnessRegistryError,
    RegisterHarnessRevisionRequest, UpdateHarnessRequest,
};
use llm_contracts::{JsonObject, ModelId, ProviderId};
use serde_json::json;

use crate::harness::model_catalog::{SUPPORTED_PROVIDERS, model_ids};

pub const ENVIRONMENT_HARNESS_ID: &str = "environment";
pub const ENVIRONMENT_HARNESS_REVISION_ID: &str = "environment-2026-09-01-configurable-web-tools";
pub const ENVIRONMENT_HARNESS_DESCRIPTOR: HarnessDescriptor = HarnessDescriptor::new(
    ENVIRONMENT_HARNESS_ID,
    "environment",
    "environment-harness-turns-v1",
);

/// Idempotently provisions the harness revision implemented by this binary.
pub async fn ensure_environment_harness(
    config: AgentControlServiceConfig,
) -> Result<(), HarnessRegistryError> {
    let client = HarnessRegistryClient::new(config)?;
    client.create_harness(&environment_harness()).await?;
    client
        .update_harness(ENVIRONMENT_HARNESS_ID, &environment_harness_metadata())
        .await?;
    client
        .register_revision(ENVIRONMENT_HARNESS_ID, &environment_revision())
        .await?;
    client
        .activate_revision(ENVIRONMENT_HARNESS_ID, ENVIRONMENT_HARNESS_REVISION_ID)
        .await?;
    client.set_enabled(ENVIRONMENT_HARNESS_ID, true).await
}

fn environment_harness() -> CreateHarnessRequest {
    CreateHarnessRequest {
        harness_id: ENVIRONMENT_HARNESS_ID.to_owned(),
        slug: "environment".to_owned(),
        display_name: "Environment Harness".to_owned(),
        description: Some(environment_harness_description().to_owned()),
        supported_providers: environment_supported_providers(),
        supported_reasoning_levels: HarnessReasoningLevel::ALL.to_vec(),
    }
}

fn environment_harness_metadata() -> UpdateHarnessRequest {
    UpdateHarnessRequest {
        display_name: None,
        description: Some(Some(environment_harness_description().to_owned())),
        supported_providers: Some(environment_supported_providers()),
        supported_reasoning_levels: Some(HarnessReasoningLevel::ALL.to_vec()),
    }
}

fn environment_supported_providers() -> Vec<HarnessProvider> {
    SUPPORTED_PROVIDERS
        .iter()
        .map(|provider_id| HarnessProvider {
            provider_id: ProviderId::new(*provider_id).expect("catalog provider id is valid"),
            model_ids: model_ids(provider_id)
                .expect("supported provider has a model catalog")
                .into_iter()
                .map(|model_id| ModelId::new(model_id).expect("catalog model id is valid"))
                .collect(),
        })
        .collect()
}

fn environment_harness_description() -> &'static str {
    "Harness to create and update project execution environments."
}

fn environment_revision() -> RegisterHarnessRevisionRequest {
    let provider_model_schemas = SUPPORTED_PROVIDERS
        .iter()
        .map(|provider| {
            json!({
                "properties": {
                    "provider": {"const": provider},
                    "model_id": {
                        "enum": model_ids(provider).expect("supported provider has a model catalog")
                    }
                }
            })
        })
        .collect::<Vec<_>>();
    RegisterHarnessRevisionRequest {
        harness_revision_id: ENVIRONMENT_HARNESS_REVISION_ID.to_owned(),
        revision: "2026-09-01-configurable-web-tools".to_owned(),
        contract_version: 1,
        default_config: object(json!({
            "provider": "openai",
            "model_id": "gpt-5.6-sol",
            "reasoning_level": "high",
            "web_search_enabled": true
        })),
        config_schema: Some(object(json!({
            "$schema": "https://json-schema.org/draft/2020-12/schema",
            "type": "object",
            "properties": {
                "provider": {"enum": SUPPORTED_PROVIDERS},
                "model_id": {"type": "string"},
                "reasoning_level": {
                    "enum": ["low", "medium", "high", "xhigh", "max"]
                },
                "account_id": {"type": "string", "minLength": 1},
                "project_id": {"type": "string", "format": "uuid"},
                "web_search_enabled": {"type": "boolean", "default": true}
            },
            "oneOf": provider_model_schemas,
            "required": ["provider", "model_id", "reasoning_level"],
            "additionalProperties": false
        }))),
    }
}

fn object(value: serde_json::Value) -> JsonObject {
    value
        .as_object()
        .expect("static value is an object")
        .clone()
}
