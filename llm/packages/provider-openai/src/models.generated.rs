// @generated from the ANK model catalog. Do not edit pricing inline.

use llm_contracts::ModelPricingAbove;

/// Complete allowlist of models accepted by this provider.
pub const OPENAI_MODELS: &[OpenAiModel] = &[
    OpenAiModel {
        id: "gpt-6-astra",
        name: "GPT-6 Astra",
        pricing: ModelPricing {
            base: ModelCost {
                input: 10.0,
                output: 50.0,
                cache_read: 1.0,
                cache_write: 12.5,
            },
            above: Some(ModelPricingAbove {
                prompt_tokens: 272_000,
                cost: ModelCost {
                    input: 20.0,
                    output: 75.0,
                    cache_read: 2.0,
                    cache_write: 25.0,
                },
            }),
        },
        context_window: 1_050_000,
        max_tokens: 128_000,
    },
    OpenAiModel {
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
    OpenAiModel {
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
    OpenAiModel {
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
