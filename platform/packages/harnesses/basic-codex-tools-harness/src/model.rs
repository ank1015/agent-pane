use llm_contracts::{JsonObject, ModelRef};
use serde::{Deserialize, Serialize};
use serde_json::json;
use uuid::Uuid;

// Explicit behavior policy, separate from the provider-owned pricing catalog.
const MODELS: &[&str] = &["gpt-5.6-sol", "gpt-5.6-terra", "gpt-5.6-luna"];

pub fn supported_models() -> std::collections::BTreeMap<String, Vec<String>> {
    ["openai", "chatgpt"]
        .into_iter()
        .map(|provider| {
            (
                provider.into(),
                MODELS
                    .iter()
                    .filter(|id| provider_openai::find_model(id).is_some())
                    .map(|id| (*id).into())
                    .collect(),
            )
        })
        .collect()
}

#[derive(Clone, Copy, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ReasoningLevel {
    Low,
    Medium,
    High,
    Xhigh,
    Max,
}

pub fn provider_options(
    model: &ModelRef,
    effort: ReasoningLevel,
    session: Uuid,
) -> Result<JsonObject, String> {
    let exists = match model.provider.as_str() {
        "openai" => provider_openai::find_model(model.id.as_str()).is_some(),
        "chatgpt" => provider_chatgpt::find_model(model.id.as_str()).is_some(),
        _ => false,
    };
    if !exists || !MODELS.contains(&model.id.as_str()) {
        return Err("Unsupported model or missing Codex tool capability policy".into());
    }
    Ok(json!({
        "store": false,
        "include": ["reasoning.encrypted_content"],
        "prompt_cache_key": session.to_string(),
        "tool_choice": "auto",
        "parallel_tool_calls": false,
        "reasoning": {"effort": effort},
        "text": {"verbosity": "low"}
    })
    .as_object()
    .unwrap()
    .clone())
}
