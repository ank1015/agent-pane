use llm_contracts::{JsonObject, ModelRef};
use serde::{Deserialize, Serialize};
use serde_json::json;
use uuid::Uuid;
#[derive(Clone, Copy, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ReasoningLevel {
    Low,
    Medium,
    High,
    Xhigh,
    Max,
}
impl ReasoningLevel {
    fn as_str(self) -> &'static str {
        match self {
            Self::Low => "low",
            Self::Medium => "medium",
            Self::High => "high",
            Self::Xhigh => "xhigh",
            Self::Max => "max",
        }
    }
}

/// Pi-style policy. Provider catalogs own model IDs and output capacities.
pub fn provider_options(
    model_ref: &ModelRef,
    reasoning_level: ReasoningLevel,
    session: Uuid,
) -> Result<JsonObject, String> {
    let id = model_ref.id.as_str();
    let effort = reasoning_level.as_str();
    let options = match model_ref.provider.as_str() {
        "openai" | "chatgpt" => {
            let model = provider_openai::find_model(id).ok_or("Unknown OpenAI/ChatGPT model")?;
            let mut options = json!({"store":false,"reasoning":{"effort":effort,"summary":"auto"},
                    "include":["reasoning.encrypted_content"],"prompt_cache_key":session.to_string()});
            if model_ref.provider.as_str() == "openai" {
                options["prompt_cache_options"] = json!({"mode":"implicit"});
                options["max_output_tokens"] = json!(model.max_tokens);
            } else {
                options["text"] = json!({"verbosity":"low"});
                options["tool_choice"] = json!("auto");
                // Multiple calls may be emitted; the harness executes them sequentially.
                options["parallel_tool_calls"] = json!(true);
            }
            options
        }
        "fireworks" => {
            let model = provider_fireworks::find_model(id).ok_or("Unknown Fireworks model")?;
            let effort = provider_fireworks::reasoning_effort(id, effort)
                .ok_or("Missing reasoning mapping for Fireworks model")?;
            json!({"reasoning_effort":effort,"prompt_cache_key":session.to_string(),"max_tokens":model.max_tokens})
        }
        _ => return Err("Unsupported provider".into()),
    };
    Ok(options.as_object().expect("object literal").clone())
}
