use llm_contracts::{ModelPricing, ModelRef};
use serde::Serialize;

use crate::account::ProviderKind;

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ProviderCapability {
    Complete,
    Search,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct ProviderDescriptor {
    pub id: ProviderKind,
    pub capabilities: &'static [ProviderCapability],
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ModelInputType {
    Text,
    Image,
}

#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct CatalogModel {
    pub id: String,
    pub provider: ProviderKind,
    pub name: String,
    pub input: Vec<ModelInputType>,
    pub pricing: ModelPricing,
    pub context_window: u64,
    pub max_tokens: u64,
}

const COMPLETE_AND_SEARCH: &[ProviderCapability] =
    &[ProviderCapability::Complete, ProviderCapability::Search];
const COMPLETE_ONLY: &[ProviderCapability] = &[ProviderCapability::Complete];

#[must_use]
pub fn providers() -> Vec<ProviderDescriptor> {
    vec![
        ProviderDescriptor {
            id: ProviderKind::Openai,
            capabilities: COMPLETE_AND_SEARCH,
        },
        ProviderDescriptor {
            id: ProviderKind::Chatgpt,
            capabilities: COMPLETE_AND_SEARCH,
        },
        ProviderDescriptor {
            id: ProviderKind::Fireworks,
            capabilities: COMPLETE_ONLY,
        },
    ]
}

#[must_use]
pub fn all_models(provider_filter: Option<ProviderKind>) -> Vec<CatalogModel> {
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
                matches!(
                    model.id,
                    "accounts/fireworks/models/glm-5p3-flash" | "accounts/fireworks/models/kimi-k3"
                ),
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
    }
}

#[must_use]
pub fn supports(provider: ProviderKind, capability: ProviderCapability) -> bool {
    match (provider, capability) {
        (_, ProviderCapability::Complete)
        | (ProviderKind::Openai | ProviderKind::Chatgpt, ProviderCapability::Search) => true,
        (ProviderKind::Fireworks, ProviderCapability::Search) => false,
    }
}

fn model_from_parts(
    provider: ProviderKind,
    id: &str,
    name: &str,
    pricing: ModelPricing,
    context_window: u64,
    max_tokens: u64,
    supports_images: bool,
) -> CatalogModel {
    let mut input = vec![ModelInputType::Text];
    if supports_images {
        input.push(ModelInputType::Image);
    }
    CatalogModel {
        id: id.to_owned(),
        provider,
        name: name.to_owned(),
        input,
        pricing,
        context_window,
        max_tokens,
    }
}

#[cfg(test)]
mod tests {
    use super::{ProviderCapability, all_models, providers, supports};
    use crate::account::ProviderKind;

    #[test]
    fn registry_contains_only_the_three_implemented_providers() {
        assert_eq!(
            providers()
                .into_iter()
                .map(|provider| provider.id)
                .collect::<Vec<_>>(),
            vec![
                ProviderKind::Openai,
                ProviderKind::Chatgpt,
                ProviderKind::Fireworks,
            ]
        );
        assert!(supports(ProviderKind::Openai, ProviderCapability::Search));
        assert!(supports(ProviderKind::Chatgpt, ProviderCapability::Search));
        assert!(!supports(
            ProviderKind::Fireworks,
            ProviderCapability::Search
        ));
    }

    #[test]
    fn aggregated_catalog_has_models_for_every_provider() {
        let models = all_models(None);
        for provider in ["openai", "chatgpt", "fireworks"] {
            assert!(
                models
                    .iter()
                    .any(|model| model.provider.as_str() == provider)
            );
        }
        let fireworks = all_models(Some(ProviderKind::Fireworks));
        assert!(
            fireworks
                .iter()
                .find(|model| model.id == "accounts/fireworks/models/kimi-k3")
                .unwrap()
                .input
                .contains(&super::ModelInputType::Image)
        );
        assert!(
            !fireworks
                .iter()
                .find(|model| model.id == "accounts/fireworks/models/glm-5p3")
                .unwrap()
                .input
                .contains(&super::ModelInputType::Image)
        );
    }
}
