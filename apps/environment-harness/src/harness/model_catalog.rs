use provider_chatgpt::{CHATGPT_MODELS, CHATGPT_PROVIDER, find_model as find_chatgpt_model};
use provider_deepseek::{DEEPSEEK_MODELS, DEEPSEEK_PROVIDER, find_model as find_deepseek_model};
use provider_fireworks::{
    FIREWORKS_MODELS, FIREWORKS_PROVIDER, find_model as find_fireworks_model,
};
use provider_openai::{OPENAI_MODELS, OPENAI_PROVIDER, find_model as find_openai_model};

pub const SUPPORTED_PROVIDERS: &[&str] = &[
    OPENAI_PROVIDER,
    CHATGPT_PROVIDER,
    FIREWORKS_PROVIDER,
    DEEPSEEK_PROVIDER,
];

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct ModelCatalogEntry {
    pub provider: &'static str,
    pub id: &'static str,
    pub name: &'static str,
    pub context_window: u64,
    pub max_tokens: u64,
}

#[must_use]
pub fn supports_provider(provider: &str) -> bool {
    SUPPORTED_PROVIDERS.contains(&provider)
}

#[must_use]
pub fn find_model(provider: &str, model_id: &str) -> Option<ModelCatalogEntry> {
    match provider {
        OPENAI_PROVIDER => {
            let model = find_openai_model(model_id)?;
            Some(model_entry(
                OPENAI_PROVIDER,
                model.id,
                model.name,
                model.context_window,
                model.max_tokens,
            ))
        }
        CHATGPT_PROVIDER => {
            let model = find_chatgpt_model(model_id)?;
            Some(model_entry(
                CHATGPT_PROVIDER,
                model.id,
                model.name,
                model.context_window,
                model.max_tokens,
            ))
        }
        FIREWORKS_PROVIDER => {
            let model = find_fireworks_model(model_id)?;
            Some(model_entry(
                FIREWORKS_PROVIDER,
                model.id,
                model.name,
                model.context_window,
                model.max_tokens,
            ))
        }
        DEEPSEEK_PROVIDER => {
            let model = find_deepseek_model(model_id)?;
            Some(model_entry(
                DEEPSEEK_PROVIDER,
                model.id,
                model.name,
                model.context_window,
                model.max_tokens,
            ))
        }
        _ => None,
    }
}

#[must_use]
pub fn model_ids(provider: &str) -> Option<Vec<&'static str>> {
    match provider {
        OPENAI_PROVIDER => Some(OPENAI_MODELS.iter().map(|model| model.id).collect()),
        CHATGPT_PROVIDER => Some(CHATGPT_MODELS.iter().map(|model| model.id).collect()),
        FIREWORKS_PROVIDER => Some(FIREWORKS_MODELS.iter().map(|model| model.id).collect()),
        DEEPSEEK_PROVIDER => Some(DEEPSEEK_MODELS.iter().map(|model| model.id).collect()),
        _ => None,
    }
}

fn model_entry(
    provider: &'static str,
    id: &'static str,
    name: &'static str,
    context_window: u64,
    max_tokens: u64,
) -> ModelCatalogEntry {
    ModelCatalogEntry {
        provider,
        id,
        name,
        context_window,
        max_tokens,
    }
}

#[cfg(test)]
mod tests {
    use provider_chatgpt::CHATGPT_MODELS;
    use provider_deepseek::DEEPSEEK_MODELS;
    use provider_fireworks::FIREWORKS_MODELS;
    use provider_openai::OPENAI_MODELS;

    use super::{SUPPORTED_PROVIDERS, find_model, model_ids, supports_provider};

    #[test]
    fn resolves_models_from_the_provider_owned_catalogs() {
        assert_eq!(CHATGPT_MODELS, OPENAI_MODELS);
        for provider in ["openai", "chatgpt"] {
            let model = find_model(provider, "gpt-5.6-terra").expect("provider catalog model");
            assert_eq!(model.provider, provider);
            assert_eq!(model.name, "GPT-5.6 Terra");
            assert_eq!(model.context_window, 1_050_000);
            assert_eq!(model.max_tokens, 128_000);
        }
    }

    #[test]
    fn rejects_unknown_providers_and_models() {
        assert!(supports_provider("openai"));
        assert!(supports_provider("chatgpt"));
        assert!(supports_provider("fireworks"));
        assert!(supports_provider("deepseek"));
        assert!(!supports_provider("anthropic"));
        assert!(find_model("anthropic", "gpt-5.6-sol").is_none());
        assert!(find_model("chatgpt", "gpt-5.5").is_none());
        assert_eq!(
            SUPPORTED_PROVIDERS,
            ["openai", "chatgpt", "fireworks", "deepseek"]
        );
        assert_eq!(
            model_ids("chatgpt").expect("ChatGPT models"),
            ["gpt-5.6-sol", "gpt-5.6-terra", "gpt-5.6-luna"]
        );
        assert_eq!(
            model_ids("fireworks").expect("Fireworks models"),
            FIREWORKS_MODELS
                .iter()
                .map(|model| model.id)
                .collect::<Vec<_>>()
        );
        let fireworks = find_model("fireworks", "accounts/fireworks/models/kimi-k3")
            .expect("Fireworks catalog model");
        assert_eq!(fireworks.provider, "fireworks");
        assert_eq!(fireworks.name, "Kimi K3");
        assert_eq!(fireworks.context_window, 1_048_576);
        assert_eq!(
            model_ids("deepseek").expect("DeepSeek models"),
            DEEPSEEK_MODELS
                .iter()
                .map(|model| model.id)
                .collect::<Vec<_>>()
        );
        let deepseek = find_model("deepseek", "deepseek-v4-flash").expect("DeepSeek model");
        assert_eq!(deepseek.provider, "deepseek");
        assert_eq!(deepseek.name, "DeepSeek V4 Flash 0731");
        assert_eq!(deepseek.context_window, 1_048_576);
        assert_eq!(deepseek.max_tokens, 384_000);
    }
}
