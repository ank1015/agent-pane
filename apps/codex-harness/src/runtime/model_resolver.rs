use llm_contracts::{JsonObject, ModelId, ModelRef, ProviderId};
use serde_json::json;
use uuid::Uuid;

use super::{CodexHarnessConfig, CodexProvider};

#[derive(Clone, Debug, PartialEq)]
pub struct CodexModelConfig {
    pub model: ModelRef,
    pub provider_options: JsonObject,
}

#[must_use]
pub fn resolve_model_config(config: &CodexHarnessConfig, session_id: Uuid) -> CodexModelConfig {
    let provider = match config.provider {
        CodexProvider::OpenAi => provider_openai::OPENAI_PROVIDER,
        CodexProvider::ChatGpt => provider_chatgpt::CHATGPT_PROVIDER,
    };
    let profile = config.model.profile();
    let provider_options = JsonObject::from_iter([
        ("store".to_owned(), json!(false)),
        (
            "reasoning".to_owned(),
            json!({
                "effort": config.reasoning_level.as_str(),
                "context": "all_turns"
            }),
        ),
        ("include".to_owned(), json!(["reasoning.encrypted_content"])),
        ("tool_choice".to_owned(), json!("auto")),
        // Responses Lite (used by Codex code mode) rejects parallel tool
        // calling. `exec` and `wait` are therefore always emitted serially.
        ("parallel_tool_calls".to_owned(), json!(false)),
        ("prompt_cache_key".to_owned(), json!(session_id.to_string())),
        ("text".to_owned(), json!({ "verbosity": "low" })),
        (
            provider_openai::CODEX_RESPONSES_LITE_OPTION.to_owned(),
            json!(true),
        ),
    ]);

    CodexModelConfig {
        model: ModelRef {
            provider: ProviderId::new(provider).expect("built-in provider ID is valid"),
            id: ModelId::new(config.model.as_str()).expect("catalog model ID is valid"),
            name: Some(profile.name.to_owned()),
        },
        provider_options,
    }
}

#[cfg(test)]
mod tests {
    use llm_contracts::JsonObject;
    use serde_json::{Value, json};
    use uuid::Uuid;

    use super::resolve_model_config;
    use crate::runtime::CodexHarnessConfig;

    #[test]
    fn matches_codex_responses_options_for_every_model_and_provider() {
        let session_id = Uuid::now_v7();
        for provider in ["openai", "chatgpt"] {
            for model in ["gpt-5.6-sol", "gpt-5.6-terra", "gpt-5.6-luna"] {
                let config = CodexHarnessConfig::from_resolved(&resolved(provider, model))
                    .expect("supported config");
                let model_config = resolve_model_config(&config, session_id);
                assert_eq!(model_config.model.provider.as_str(), provider);
                assert_eq!(model_config.model.id.as_str(), model);
                assert_eq!(
                    model_config.provider_options["reasoning"],
                    json!({"effort": "xhigh", "context": "all_turns"})
                );
                assert_eq!(
                    model_config.provider_options[provider_openai::CODEX_RESPONSES_LITE_OPTION],
                    json!(true)
                );
                assert_eq!(model_config.provider_options["store"], json!(false));
                assert_eq!(
                    model_config.provider_options["include"],
                    json!(["reasoning.encrypted_content"])
                );
                assert_eq!(model_config.provider_options["tool_choice"], json!("auto"));
                assert_eq!(
                    model_config.provider_options["parallel_tool_calls"],
                    json!(false)
                );
                assert_eq!(
                    model_config.provider_options["text"],
                    json!({"verbosity": "low"})
                );
                assert_eq!(
                    model_config.provider_options["prompt_cache_key"],
                    json!(session_id)
                );
                assert!(
                    !model_config
                        .provider_options
                        .contains_key("max_output_tokens")
                );
                assert!(
                    model_config.provider_options["reasoning"]
                        .get("summary")
                        .is_none()
                );
            }
        }
    }

    fn resolved(provider: &str, model: &str) -> JsonObject {
        let Value::Object(value) = json!({
            "provider": provider,
            "model_id": model,
            "reasoning_level": "xhigh",
            "execution": {
                "machine_id": "machine-a",
                "workspace_root_id": "root",
                "cwd": "."
            }
        }) else {
            unreachable!()
        };
        value
    }
}
