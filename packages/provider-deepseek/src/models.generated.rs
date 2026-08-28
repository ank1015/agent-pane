// @generated from DeepSeek's V4 catalog and August 2026 pricing announcement.

/// Complete allowlist of models accepted by this provider.
pub const DEEPSEEK_MODELS: &[DeepSeekModel] = &[
    DeepSeekModel {
        id: "deepseek-v4-flash",
        name: "DeepSeek V4 Flash 0731",
        pricing: ModelPricing {
            base: ModelCost {
                input: 0.22,
                output: 0.66,
                cache_read: 0.007,
                cache_write: 0.0,
            },
            above: None,
        },
        peak_pricing: ModelCost {
            input: 0.44,
            output: 1.32,
            cache_read: 0.014,
            cache_write: 0.0,
        },
        context_window: 1_048_576,
        max_tokens: 384_000,
        supports_images: false,
    },
    DeepSeekModel {
        id: "deepseek-v4-pro",
        name: "DeepSeek V4 Pro 0813",
        pricing: ModelPricing {
            base: ModelCost {
                input: 0.66,
                output: 1.98,
                cache_read: 0.022,
                cache_write: 0.0,
            },
            above: None,
        },
        peak_pricing: ModelCost {
            input: 1.32,
            output: 3.96,
            cache_read: 0.044,
            cache_write: 0.0,
        },
        context_window: 1_048_576,
        max_tokens: 384_000,
        supports_images: false,
    },
    DeepSeekModel {
        id: "deepseek-v4-flash-vision-exp",
        name: "DeepSeek V4 Flash Vision Exp",
        pricing: ModelPricing {
            base: ModelCost {
                input: 0.22,
                output: 0.66,
                cache_read: 0.007,
                cache_write: 0.0,
            },
            above: None,
        },
        peak_pricing: ModelCost {
            input: 0.44,
            output: 1.32,
            cache_read: 0.014,
            cache_write: 0.0,
        },
        context_window: 1_048_576,
        max_tokens: 384_000,
        supports_images: true,
    },
];
