use llm_contracts::{JsonObject, ModelId, ModelRef, ProviderId};
use serde_json::json;

use super::model_catalog::MODEL_CATALOG;

pub const SUPPORTED_REASONING_LEVELS: &[&str] = &["low", "medium", "high", "xhigh", "max"];

#[derive(Clone, Debug, PartialEq)]
pub struct ModelConfig {
    pub model: ModelRef,
    pub provider_options: JsonObject,
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
    session_id: &str,
) -> Result<ModelConfig, ModelResolverError> {
    if !MODEL_CATALOG.iter().any(|model| model.provider == provider) {
        return Err(ModelResolverError::UnsupportedProvider(provider.to_owned()));
    }

    let model = MODEL_CATALOG
        .iter()
        .find(|model| model.provider == provider && model.id == model_id)
        .ok_or_else(|| ModelResolverError::UnsupportedModel {
            provider: provider.to_owned(),
            model_id: model_id.to_owned(),
        })?;

    if !SUPPORTED_REASONING_LEVELS.contains(&reasoning_level) {
        return Err(ModelResolverError::UnsupportedReasoningLevel(
            reasoning_level.to_owned(),
        ));
    }

    let provider_options = JsonObject::from_iter([
        ("store".to_owned(), json!(false)),
        ("prompt_cache_key".to_owned(), json!(session_id)),
        (
            "prompt_cache_options".to_owned(),
            json!({ "mode": "implicit" }),
        ),
        (
            "reasoning".to_owned(),
            json!({
                "effort": reasoning_level,
                "summary": "auto"
            }),
        ),
        ("include".to_owned(), json!(["reasoning.encrypted_content"])),
    ]);

    Ok(ModelConfig {
        model: ModelRef {
            provider: ProviderId::new(model.provider).expect("catalog provider is valid"),
            id: ModelId::new(model.id).expect("catalog model id is valid"),
            name: Some(model.name.to_owned()),
        },
        provider_options,
    })
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::{ModelResolverError, SUPPORTED_REASONING_LEVELS, get_model_config};

    #[test]
    fn resolves_catalog_model_and_pi_reasoning_options() {
        let config = get_model_config("openai", "gpt-5.6-terra", "high", "session-123")
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
    }

    #[test]
    fn accepts_every_supported_reasoning_level() {
        for reasoning_level in SUPPORTED_REASONING_LEVELS {
            let config = get_model_config("openai", "gpt-5.6-luna", reasoning_level, "session-123")
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
            get_model_config("anthropic", "gpt-5.6-sol", "medium", "session-123"),
            Err(ModelResolverError::UnsupportedProvider(
                "anthropic".to_owned()
            ))
        );
    }

    #[test]
    fn rejects_model_outside_provider_catalog() {
        assert_eq!(
            get_model_config("openai", "gpt-5.5", "medium", "session-123"),
            Err(ModelResolverError::UnsupportedModel {
                provider: "openai".to_owned(),
                model_id: "gpt-5.5".to_owned(),
            })
        );
    }

    #[test]
    fn rejects_unsupported_reasoning_level() {
        assert_eq!(
            get_model_config("openai", "gpt-5.6-sol", "minimal", "session-123"),
            Err(ModelResolverError::UnsupportedReasoningLevel(
                "minimal".to_owned()
            ))
        );
    }
}
