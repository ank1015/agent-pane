use agent_harness_sdk::{
    AgentControlServiceConfig, CreateHarnessRequest, HarnessDescriptor, HarnessProvider,
    HarnessReasoningLevel, HarnessRegistryClient, HarnessRegistryError,
    RegisterHarnessRevisionRequest, UpdateHarnessRequest,
};
use llm_contracts::{JsonObject, ModelId, ProviderId};
use serde_json::json;

use crate::harness::model_catalog::{SUPPORTED_PROVIDERS, model_ids};

pub const PI_HARNESS_ID: &str = "pi";
pub const PI_HARNESS_REVISION_ID: &str = "pi-2026-08-28-deepseek";
pub const PI_HARNESS_DESCRIPTOR: HarnessDescriptor =
    HarnessDescriptor::new(PI_HARNESS_ID, "pi", "pi-harness-turns-v1");

/// Idempotently provisions the harness revision implemented by this binary.
pub async fn ensure_pi_harness(
    config: AgentControlServiceConfig,
) -> Result<(), HarnessRegistryError> {
    let client = HarnessRegistryClient::new(config)?;
    client.create_harness(&pi_harness()).await?;
    client
        .update_harness(PI_HARNESS_ID, &pi_harness_metadata())
        .await?;
    client
        .register_revision(PI_HARNESS_ID, &pi_revision())
        .await?;
    client
        .activate_revision(PI_HARNESS_ID, PI_HARNESS_REVISION_ID)
        .await?;
    client.set_enabled(PI_HARNESS_ID, true).await
}

fn pi_harness() -> CreateHarnessRequest {
    CreateHarnessRequest {
        harness_id: PI_HARNESS_ID.to_owned(),
        slug: "pi".to_owned(),
        display_name: "Pi Coding Agent".to_owned(),
        description: Some(pi_harness_description().to_owned()),
        supported_providers: pi_supported_providers(),
        supported_reasoning_levels: HarnessReasoningLevel::ALL.to_vec(),
    }
}

fn pi_harness_metadata() -> UpdateHarnessRequest {
    UpdateHarnessRequest {
        display_name: None,
        description: Some(Some(pi_harness_description().to_owned())),
        supported_providers: Some(pi_supported_providers()),
        supported_reasoning_levels: Some(HarnessReasoningLevel::ALL.to_vec()),
    }
}

fn pi_supported_providers() -> Vec<HarnessProvider> {
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

fn pi_harness_description() -> &'static str {
    "Minimalistic pi agent with four tools. Single agent, Single machine."
}

fn pi_revision() -> RegisterHarnessRevisionRequest {
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
        harness_revision_id: PI_HARNESS_REVISION_ID.to_owned(),
        revision: "2026-08-28-deepseek".to_owned(),
        contract_version: 1,
        default_config: object(json!({
            "provider": "openai",
            "model_id": "gpt-5.6-sol",
            "reasoning_level": "high",
            "is_replaced": false
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
                "execution": {
                    "type": "object",
                    "properties": {
                        "machine_id": {"type": "string", "minLength": 1},
                        "workspace_root_id": {"type": "string", "minLength": 1},
                        "cwd": {"type": "string", "minLength": 1}
                    },
                    "required": ["machine_id", "workspace_root_id", "cwd"],
                    "additionalProperties": false
                },
                "account_id": {"type": "string", "minLength": 1},
                "external_prompt": {"type": ["string", "null"]},
                "is_replaced": {"type": "boolean"}
            },
            "oneOf": provider_model_schemas,
            "required": ["provider", "model_id", "reasoning_level", "is_replaced"],
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
