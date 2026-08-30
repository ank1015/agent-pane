use agent_harness_sdk::{
    AgentControlServiceConfig, CreateHarnessRequest, HarnessDescriptor, HarnessProvider,
    HarnessRegistryClient, HarnessRegistryError, RegisterHarnessRevisionRequest,
    UpdateHarnessRequest,
};
use llm_contracts::{JsonObject, ModelId, ProviderId};
use serde_json::json;

use crate::runtime::{SUPPORTED_MODEL_IDS, SUPPORTED_PROVIDER_IDS, SUPPORTED_REASONING_LEVELS};

pub const CODEX_HARNESS_ID: &str = "codex";
pub const CODEX_HARNESS_REVISION_ID: &str = "codex-2026-08-31-web-search-v2";
pub const CODEX_HARNESS_DESCRIPTOR: HarnessDescriptor =
    HarnessDescriptor::new(CODEX_HARNESS_ID, "codex", "codex-harness-turns-v1");

/// Idempotently provisions the revision represented by this binary.
pub async fn ensure_codex_harness(
    config: AgentControlServiceConfig,
) -> Result<(), HarnessRegistryError> {
    let client = HarnessRegistryClient::new(config)?;
    client.create_harness(&create_request()).await?;
    client
        .update_harness(CODEX_HARNESS_ID, &update_request())
        .await?;
    client
        .register_revision(CODEX_HARNESS_ID, &revision_request())
        .await?;
    client
        .activate_revision(CODEX_HARNESS_ID, CODEX_HARNESS_REVISION_ID)
        .await?;
    client.set_enabled(CODEX_HARNESS_ID, true).await
}

fn create_request() -> CreateHarnessRequest {
    CreateHarnessRequest {
        harness_id: CODEX_HARNESS_ID.to_owned(),
        slug: "codex".to_owned(),
        display_name: "Codex Coding Agent".to_owned(),
        description: Some(description().to_owned()),
        supported_providers: supported_providers(),
    }
}

fn update_request() -> UpdateHarnessRequest {
    UpdateHarnessRequest {
        display_name: None,
        description: Some(Some(description().to_owned())),
        supported_providers: Some(supported_providers()),
    }
}

fn supported_providers() -> Vec<HarnessProvider> {
    SUPPORTED_PROVIDER_IDS
        .iter()
        .map(|provider| HarnessProvider {
            provider_id: ProviderId::new(*provider).expect("provider ID is valid"),
            model_ids: SUPPORTED_MODEL_IDS
                .iter()
                .map(|model| ModelId::new(*model).expect("model ID is valid"))
                .collect(),
        })
        .collect()
}

fn description() -> &'static str {
    "Basic Codex harness with Codex-compatible tools and agent behavior."
}

fn revision_request() -> RegisterHarnessRevisionRequest {
    RegisterHarnessRevisionRequest {
        harness_revision_id: CODEX_HARNESS_REVISION_ID.to_owned(),
        revision: "2026-08-31-web-search-v2".to_owned(),
        contract_version: 1,
        default_config: object(json!({
            "provider": "openai",
            "model_id": "gpt-5.6-sol",
            "reasoning_level": "low",
            "web_search_enabled": true
        })),
        config_schema: Some(object(json!({
            "$schema": "https://json-schema.org/draft/2020-12/schema",
            "type": "object",
            "properties": {
                "provider": {"enum": SUPPORTED_PROVIDER_IDS},
                "model_id": {"enum": SUPPORTED_MODEL_IDS},
                "reasoning_level": {"enum": SUPPORTED_REASONING_LEVELS},
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
                "account_id": {"type": "string", "format": "uuid"},
                "external_prompt": {"type": ["string", "null"]},
                "is_replaced": {"type": "boolean"},
                "web_search_enabled": {"type": "boolean", "default": true}
            },
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
