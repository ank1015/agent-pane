use llm_contracts::{Model, ModelInputType, ModelRef, ProviderId};

use crate::account::ProviderKind;

#[must_use]
pub fn all_models(provider_filter: Option<ProviderKind>) -> Vec<Model> {
    let mut models = Vec::new();
    if provider_filter.is_none_or(|provider| provider == ProviderKind::Openai) {
        models.extend(provider_openai::OPENAI_MODELS.iter().map(|model| {
            model_from_parts(
                ProviderKind::Openai,
                model.id,
                model.name,
                model.pricing,
                model.context_window,
                model.max_tokens,
                true,
            )
        }));
    }
    if provider_filter.is_none_or(|provider| provider == ProviderKind::Chatgpt) {
        models.extend(provider_chatgpt::CHATGPT_MODELS.iter().map(|model| {
            model_from_parts(
                ProviderKind::Chatgpt,
                model.id,
                model.name,
                model.pricing,
                model.context_window,
                model.max_tokens,
                true,
            )
        }));
    }
    if provider_filter.is_none_or(|provider| provider == ProviderKind::Fireworks) {
        models.extend(provider_fireworks::FIREWORKS_MODELS.iter().map(|model| {
            model_from_parts(
                ProviderKind::Fireworks,
                model.id,
                model.name,
                model.pricing,
                model.context_window,
                model.max_tokens,
                true,
            )
        }));
    }
    if provider_filter.is_none_or(|provider| provider == ProviderKind::Anthropic) {
        models.extend(provider_anthropic::ANTHROPIC_MODELS.iter().map(|model| {
            model_from_parts(
                ProviderKind::Anthropic,
                model.id,
                model.name,
                model.pricing,
                model.context_window,
                model.max_tokens,
                true,
            )
        }));
    }
    if provider_filter.is_none_or(|provider| provider == ProviderKind::Openrouter) {
        models.extend(provider_openrouter::OPENROUTER_MODELS.iter().map(|model| {
            model_from_parts(
                ProviderKind::Openrouter,
                model.id,
                model.name,
                model.pricing,
                model.context_window,
                model.max_tokens,
                true,
            )
        }));
    }
    if provider_filter.is_none_or(|provider| provider == ProviderKind::Deepseek) {
        models.extend(provider_deepseek::DEEPSEEK_MODELS.iter().map(|model| {
            model_from_parts(
                ProviderKind::Deepseek,
                model.id,
                model.name,
                model.pricing,
                model.context_window,
                model.max_tokens,
                model.supports_images,
            )
        }));
    }
    models
}

#[must_use]
pub fn contains(model: &ModelRef) -> bool {
    let Ok(provider) = model.provider.as_str().parse::<ProviderKind>() else {
        return false;
    };
    let id = model.id.as_str();
    match provider {
        ProviderKind::Openai => provider_openai::find_model(id).is_some(),
        ProviderKind::Chatgpt => provider_chatgpt::find_model(id).is_some(),
        ProviderKind::Fireworks => provider_fireworks::find_model(id).is_some(),
        ProviderKind::Anthropic => provider_anthropic::find_model(id).is_some(),
        ProviderKind::Openrouter => provider_openrouter::find_model(id).is_some(),
        ProviderKind::Deepseek => provider_deepseek::find_model(id).is_some(),
    }
}

fn model_from_parts(
    provider: ProviderKind,
    id: &str,
    name: &str,
    pricing: llm_contracts::ModelPricing,
    context_window: u64,
    max_tokens: u64,
    supports_images: bool,
) -> Model {
    let mut input = vec![ModelInputType::Text];
    if supports_images {
        input.push(ModelInputType::Image);
    }
    Model {
        id: llm_contracts::ModelId::new(id).expect("catalog model id is valid"),
        provider: ProviderId::new(provider.as_str()).expect("catalog provider id is valid"),
        name: Some(name.to_owned()),
        input,
        pricing: Some(pricing),
        context_window: Some(context_window),
        max_tokens: Some(max_tokens),
    }
}

#[cfg(test)]
mod tests {
    use super::all_models;

    #[test]
    fn aggregated_catalog_has_every_supported_provider() {
        let models = all_models(None);
        for provider in [
            "openai",
            "chatgpt",
            "fireworks",
            "anthropic",
            "openrouter",
            "deepseek",
        ] {
            assert!(
                models
                    .iter()
                    .any(|model| model.provider.as_str() == provider)
            );
        }
    }
}
