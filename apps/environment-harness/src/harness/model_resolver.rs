use llm_contracts::{JsonObject, ModelId, ModelRef, ProviderId};
use provider_chatgpt::CHATGPT_PROVIDER;
use provider_deepseek::{DEEPSEEK_PROVIDER, reasoning_effort as deepseek_reasoning_effort};
use provider_fireworks::{FIREWORKS_PROVIDER, reasoning_effort as fireworks_reasoning_effort};
use provider_openai::OPENAI_PROVIDER;
use serde_json::json;

use super::model_catalog::{find_model, supports_provider};

pub const SUPPORTED_REASONING_LEVELS: &[&str] = &["low", "medium", "high", "xhigh", "max"];

#[derive(Clone, Debug, PartialEq)]
pub struct ModelConfig {
    pub model: ModelRef,
    pub provider_options: JsonObject,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum RequestKind<'a> {
    Turn { session_id: &'a str },
    Compaction { max_output_tokens: u64 },
}

#[derive(Debug, Eq, PartialEq, thiserror::Error)]
pub enum ModelResolverError {
    #[error("unsupported provider {0:?}")]
    UnsupportedProvider(String),
    #[error("unsupported model {model_id:?} for provider {provider:?}")]
    UnsupportedModel { provider: String, model_id: String },
    #[error("unsupported reasoning level {0:?}")]
    UnsupportedReasoningLevel(String),
}

pub fn get_model_config(
    provider: &str,
    model_id: &str,
    reasoning_level: &str,
    request_kind: RequestKind<'_>,
) -> Result<ModelConfig, ModelResolverError> {
    if !supports_provider(provider) {
        return Err(ModelResolverError::UnsupportedProvider(provider.to_owned()));
    }

    let model =
        find_model(provider, model_id).ok_or_else(|| ModelResolverError::UnsupportedModel {
            provider: provider.to_owned(),
            model_id: model_id.to_owned(),
        })?;

    if !SUPPORTED_REASONING_LEVELS.contains(&reasoning_level) {
        return Err(ModelResolverError::UnsupportedReasoningLevel(
            reasoning_level.to_owned(),
        ));
    }

    let provider_options = match provider {
        OPENAI_PROVIDER => openai_options(model.max_tokens, reasoning_level, request_kind),
        CHATGPT_PROVIDER => chatgpt_options(reasoning_level, request_kind),
        FIREWORKS_PROVIDER => {
            fireworks_options(model.id, model.max_tokens, reasoning_level, request_kind)
        }
        DEEPSEEK_PROVIDER => {
            deepseek_options(model.id, model.max_tokens, reasoning_level, request_kind)
        }
        _ => unreachable!("supported providers are exhaustively matched"),
    };

    Ok(ModelConfig {
        model: ModelRef {
            provider: ProviderId::new(model.provider).expect("catalog provider is valid"),
            id: ModelId::new(model.id).expect("catalog model id is valid"),
            name: Some(model.name.to_owned()),
        },
        provider_options,
    })
}

fn openai_options(
    model_max_tokens: u64,
    reasoning_level: &str,
    request_kind: RequestKind<'_>,
) -> JsonObject {
    let mut provider_options = responses_options(reasoning_level);
    match request_kind {
        RequestKind::Turn { session_id } => {
            provider_options.insert("prompt_cache_key".to_owned(), json!(session_id));
            provider_options.insert(
                "prompt_cache_options".to_owned(),
                json!({ "mode": "implicit" }),
            );
            provider_options.insert("max_output_tokens".to_owned(), json!(model_max_tokens));
        }
        RequestKind::Compaction { max_output_tokens } => {
            provider_options.insert(
                "prompt_cache_options".to_owned(),
                json!({ "mode": "explicit" }),
            );
            provider_options.insert("max_output_tokens".to_owned(), json!(max_output_tokens));
        }
    }
    provider_options
}

fn chatgpt_options(reasoning_level: &str, request_kind: RequestKind<'_>) -> JsonObject {
    let mut provider_options = responses_options(reasoning_level);
    match request_kind {
        RequestKind::Turn { session_id } => {
            provider_options.insert("prompt_cache_key".to_owned(), json!(session_id));
        }
        RequestKind::Compaction { .. } => {}
    }
    provider_options.insert("text".to_owned(), json!({ "verbosity": "low" }));
    provider_options.insert("tool_choice".to_owned(), json!("auto"));
    provider_options.insert("parallel_tool_calls".to_owned(), json!(true));
    provider_options
}

fn fireworks_options(
    model_id: &str,
    model_max_tokens: u64,
    reasoning_level: &str,
    request_kind: RequestKind<'_>,
) -> JsonObject {
    let reasoning_effort = fireworks_reasoning_effort(model_id, reasoning_level).expect(
        "every catalog Fireworks model has a mapping for every Environment reasoning level",
    );
    let mut provider_options =
        JsonObject::from_iter([("reasoning_effort".to_owned(), json!(reasoning_effort))]);
    match request_kind {
        RequestKind::Turn { session_id } => {
            provider_options.insert("prompt_cache_key".to_owned(), json!(session_id));
            provider_options.insert("max_tokens".to_owned(), json!(model_max_tokens));
        }
        RequestKind::Compaction { max_output_tokens } => {
            provider_options.insert("max_tokens".to_owned(), json!(max_output_tokens));
        }
    }
    provider_options
}

fn deepseek_options(
    model_id: &str,
    model_max_tokens: u64,
    reasoning_level: &str,
    request_kind: RequestKind<'_>,
) -> JsonObject {
    let reasoning_effort = deepseek_reasoning_effort(model_id, reasoning_level)
        .expect("every catalog DeepSeek model has a mapping for every Environment reasoning level");
    let max_tokens = match request_kind {
        RequestKind::Turn { .. } => model_max_tokens,
        RequestKind::Compaction { max_output_tokens } => max_output_tokens,
    };
    JsonObject::from_iter([
        ("thinking".to_owned(), json!({"type": "enabled"})),
        ("reasoning_effort".to_owned(), json!(reasoning_effort)),
        ("max_tokens".to_owned(), json!(max_tokens)),
    ])
}

fn responses_options(reasoning_level: &str) -> JsonObject {
    JsonObject::from_iter([
        ("store".to_owned(), json!(false)),
        (
            "reasoning".to_owned(),
            json!({
                "effort": reasoning_level,
                "summary": "auto"
            }),
        ),
        ("include".to_owned(), json!(["reasoning.encrypted_content"])),
    ])
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::{ModelResolverError, RequestKind, SUPPORTED_REASONING_LEVELS, get_model_config};

    #[test]
    fn resolves_catalog_model_and_environment_reasoning_options() {
        let config = get_model_config(
            "openai",
            "gpt-5.6-terra",
            "high",
            RequestKind::Turn {
                session_id: "session-123",
            },
        )
        .expect("supported config");

        assert_eq!(config.model.provider.as_str(), "openai");
        assert_eq!(config.model.id.as_str(), "gpt-5.6-terra");
        assert_eq!(config.model.name.as_deref(), Some("GPT-5.6 Terra"));
        assert_eq!(config.provider_options["store"], json!(false));
        assert_eq!(
            config.provider_options["prompt_cache_key"],
            json!("session-123")
        );
        assert_eq!(
            config.provider_options["prompt_cache_options"],
            json!({ "mode": "implicit" })
        );
        assert_eq!(
            config.provider_options["reasoning"],
            json!({ "effort": "high", "summary": "auto" })
        );
        assert_eq!(
            config.provider_options["include"],
            json!(["reasoning.encrypted_content"])
        );
        assert_eq!(config.provider_options["max_output_tokens"], json!(128_000));
    }

    #[test]
    fn resolves_chatgpt_turn_options_without_openai_only_fields() {
        let config = get_model_config(
            "chatgpt",
            "gpt-5.6-terra",
            "high",
            RequestKind::Turn {
                session_id: "session-123",
            },
        )
        .expect("supported config");

        assert_eq!(config.model.provider.as_str(), "chatgpt");
        assert_eq!(config.provider_options["store"], json!(false));
        assert_eq!(
            config.provider_options["prompt_cache_key"],
            json!("session-123")
        );
        assert_eq!(
            config.provider_options["reasoning"],
            json!({"effort": "high", "summary": "auto"})
        );
        assert_eq!(config.provider_options["text"], json!({"verbosity": "low"}));
        assert_eq!(config.provider_options["tool_choice"], json!("auto"));
        assert_eq!(config.provider_options["parallel_tool_calls"], json!(true));
        assert!(!config.provider_options.contains_key("prompt_cache_options"));
        assert!(!config.provider_options.contains_key("max_output_tokens"));
    }

    #[test]
    fn resolves_chatgpt_compaction_without_cache_or_output_limit() {
        let config = get_model_config(
            "chatgpt",
            "gpt-5.6-luna",
            "medium",
            RequestKind::Compaction {
                max_output_tokens: 13_107,
            },
        )
        .expect("supported config");

        assert!(!config.provider_options.contains_key("prompt_cache_key"));
        assert!(!config.provider_options.contains_key("prompt_cache_options"));
        assert!(!config.provider_options.contains_key("max_output_tokens"));
    }

    #[test]
    fn resolves_fireworks_turn_with_model_reasoning_and_session_affinity() {
        let config = get_model_config(
            "fireworks",
            "accounts/fireworks/models/deepseek-v4-pro-0813",
            "medium",
            RequestKind::Turn {
                session_id: "session-123",
            },
        )
        .expect("supported Fireworks config");

        assert_eq!(config.model.provider.as_str(), "fireworks");
        assert_eq!(
            config.model.id.as_str(),
            "accounts/fireworks/models/deepseek-v4-pro-0813"
        );
        assert_eq!(config.provider_options["reasoning_effort"], json!("high"));
        assert_eq!(
            config.provider_options["prompt_cache_key"],
            json!("session-123")
        );
        assert_eq!(config.provider_options["max_tokens"], json!(393_216));
        assert!(!config.provider_options.contains_key("reasoning_history"));
        assert!(!config.provider_options.contains_key("tool_choice"));
        assert!(!config.provider_options.contains_key("parallel_tool_calls"));
    }

    #[test]
    fn maps_every_environment_reasoning_level_for_every_fireworks_model() {
        let expected = [
            (
                "accounts/fireworks/models/kimi-k3",
                ["low", "medium", "high", "max", "max"],
            ),
            (
                "accounts/fireworks/models/deepseek-v4-pro-0813",
                ["high", "high", "high", "max", "max"],
            ),
            (
                "accounts/fireworks/models/qwen3p8-2p4t-a95b",
                ["low", "medium", "high", "high", "high"],
            ),
            (
                "accounts/fireworks/models/deepseek-v4-flash-0731",
                ["low", "high", "high", "max", "max"],
            ),
        ];

        for (model_id, efforts) in expected {
            for (reasoning_level, effort) in SUPPORTED_REASONING_LEVELS.iter().zip(efforts) {
                let config = get_model_config(
                    "fireworks",
                    model_id,
                    reasoning_level,
                    RequestKind::Turn {
                        session_id: "session-123",
                    },
                )
                .expect("mapped Fireworks reasoning level");
                assert_eq!(config.provider_options["reasoning_effort"], json!(effort));
            }
        }
    }

    #[test]
    fn resolves_fireworks_compaction_without_session_affinity() {
        let config = get_model_config(
            "fireworks",
            "accounts/fireworks/models/kimi-k3",
            "xhigh",
            RequestKind::Compaction {
                max_output_tokens: 13_107,
            },
        )
        .expect("supported Fireworks compaction config");

        assert_eq!(config.provider_options["reasoning_effort"], json!("max"));
        assert_eq!(config.provider_options["max_tokens"], json!(13_107));
        assert!(!config.provider_options.contains_key("prompt_cache_key"));
    }

    #[test]
    fn resolves_deepseek_turn_with_thinking_and_no_cache_options() {
        let config = get_model_config(
            "deepseek",
            "deepseek-v4-pro",
            "medium",
            RequestKind::Turn {
                session_id: "session-123",
            },
        )
        .expect("supported DeepSeek config");

        assert_eq!(config.model.provider.as_str(), "deepseek");
        assert_eq!(config.model.id.as_str(), "deepseek-v4-pro");
        assert_eq!(
            config.provider_options["thinking"],
            json!({"type": "enabled"})
        );
        assert_eq!(config.provider_options["reasoning_effort"], json!("high"));
        assert_eq!(config.provider_options["max_tokens"], json!(384_000));
        assert!(!config.provider_options.contains_key("prompt_cache_key"));
        assert!(!config.provider_options.contains_key("prompt_cache_options"));
        assert!(!config.provider_options.contains_key("tool_choice"));
        assert!(!config.provider_options.contains_key("parallel_tool_calls"));
    }

    #[test]
    fn maps_every_environment_reasoning_level_for_every_deepseek_model() {
        let expected = ["low", "high", "high", "high", "max"];

        for model_id in [
            "deepseek-v4-flash",
            "deepseek-v4-pro",
            "deepseek-v4-flash-vision-exp",
        ] {
            for (reasoning_level, effort) in SUPPORTED_REASONING_LEVELS.iter().zip(expected) {
                let config = get_model_config(
                    "deepseek",
                    model_id,
                    reasoning_level,
                    RequestKind::Turn {
                        session_id: "session-123",
                    },
                )
                .expect("mapped DeepSeek reasoning level");
                assert_eq!(config.provider_options["reasoning_effort"], json!(effort));
            }
        }
    }

    #[test]
    fn resolves_deepseek_compaction_without_cache_options() {
        let config = get_model_config(
            "deepseek",
            "deepseek-v4-flash",
            "xhigh",
            RequestKind::Compaction {
                max_output_tokens: 13_107,
            },
        )
        .expect("supported DeepSeek compaction config");

        assert_eq!(
            config.provider_options["thinking"],
            json!({"type": "enabled"})
        );
        assert_eq!(config.provider_options["reasoning_effort"], json!("high"));
        assert_eq!(config.provider_options["max_tokens"], json!(13_107));
        assert!(!config.provider_options.contains_key("prompt_cache_key"));
        assert!(!config.provider_options.contains_key("prompt_cache_options"));
    }

    #[test]
    fn accepts_every_supported_reasoning_level() {
        for reasoning_level in SUPPORTED_REASONING_LEVELS {
            let config = get_model_config(
                "openai",
                "gpt-5.6-luna",
                reasoning_level,
                RequestKind::Turn {
                    session_id: "session-123",
                },
            )
            .expect("supported reasoning level");
            assert_eq!(
                config.provider_options["reasoning"]["effort"],
                json!(reasoning_level)
            );
        }
    }

    #[test]
    fn rejects_unsupported_provider_before_model() {
        assert_eq!(
            get_model_config(
                "anthropic",
                "gpt-5.6-sol",
                "medium",
                RequestKind::Turn {
                    session_id: "session-123",
                },
            ),
            Err(ModelResolverError::UnsupportedProvider(
                "anthropic".to_owned()
            ))
        );
    }

    #[test]
    fn rejects_model_outside_provider_catalog() {
        assert_eq!(
            get_model_config(
                "openai",
                "gpt-5.5",
                "medium",
                RequestKind::Turn {
                    session_id: "session-123",
                },
            ),
            Err(ModelResolverError::UnsupportedModel {
                provider: "openai".to_owned(),
                model_id: "gpt-5.5".to_owned(),
            })
        );
    }

    #[test]
    fn rejects_unsupported_reasoning_level() {
        assert_eq!(
            get_model_config(
                "openai",
                "gpt-5.6-sol",
                "minimal",
                RequestKind::Turn {
                    session_id: "session-123",
                },
            ),
            Err(ModelResolverError::UnsupportedReasoningLevel(
                "minimal".to_owned()
            ))
        );
    }
}
