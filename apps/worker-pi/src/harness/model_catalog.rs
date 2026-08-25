use llm_contracts::{ModelCost, ModelPricing, ModelPricingAbove};

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct ModelCatalogEntry {
    pub provider: &'static str,
    pub id: &'static str,
    pub name: &'static str,
    pub pricing: ModelPricing,
    pub context_window: u64,
    pub max_tokens: u64,
}

pub const MODEL_CATALOG: &[ModelCatalogEntry] = &[
    ModelCatalogEntry {
        provider: "openai",
        id: "gpt-5.6-sol",
        name: "GPT-5.6 Sol",
        pricing: ModelPricing {
            base: ModelCost {
                input: 4.0,
                output: 20.0,
                cache_read: 0.4,
                cache_write: 5.0,
            },
            above: Some(ModelPricingAbove {
                prompt_tokens: 272_000,
                cost: ModelCost {
                    input: 8.0,
                    output: 30.0,
                    cache_read: 0.8,
                    cache_write: 10.0,
                },
            }),
        },
        context_window: 1_050_000,
        max_tokens: 128_000,
    },
    ModelCatalogEntry {
        provider: "openai",
        id: "gpt-5.6-terra",
        name: "GPT-5.6 Terra",
        pricing: ModelPricing {
            base: ModelCost {
                input: 2.0,
                output: 12.0,
                cache_read: 0.2,
                cache_write: 2.5,
            },
            above: Some(ModelPricingAbove {
                prompt_tokens: 272_000,
                cost: ModelCost {
                    input: 4.0,
                    output: 18.0,
                    cache_read: 0.4,
                    cache_write: 5.0,
                },
            }),
        },
        context_window: 1_050_000,
        max_tokens: 128_000,
    },
    ModelCatalogEntry {
        provider: "openai",
        id: "gpt-5.6-luna",
        name: "GPT-5.6 Luna",
        pricing: ModelPricing {
            base: ModelCost {
                input: 0.2,
                output: 1.2,
                cache_read: 0.02,
                cache_write: 0.25,
            },
            above: Some(ModelPricingAbove {
                prompt_tokens: 272_000,
                cost: ModelCost {
                    input: 0.4,
                    output: 1.8,
                    cache_read: 0.04,
                    cache_write: 0.5,
                },
            }),
        },
        context_window: 1_050_000,
        max_tokens: 128_000,
    },
];

#[cfg(test)]
mod tests {
    use super::MODEL_CATALOG;

    #[test]
    fn contains_the_supported_openai_models() {
        let models: Vec<_> = MODEL_CATALOG
            .iter()
            .map(|model| (model.provider, model.id))
            .collect();

        assert_eq!(
            models,
            [
                ("openai", "gpt-5.6-sol"),
                ("openai", "gpt-5.6-terra"),
                ("openai", "gpt-5.6-luna"),
            ]
        );
    }
}
