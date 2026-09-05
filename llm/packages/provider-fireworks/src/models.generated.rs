// @generated from the Fireworks standard serverless catalog. Do not edit pricing inline.

/// Complete allowlist of models accepted by this provider.
pub const FIREWORKS_MODELS: &[FireworksModel] = &[
    FireworksModel {
        id: "accounts/fireworks/models/glm-5p3-flash",
        name: "GLM 5.3 Flash",
        pricing: ModelPricing {
            base: ModelCost {
                input: 0.15,
                output: 0.50,
                cache_read: 0.03,
                cache_write: 0.0,
            },
            above: None,
        },
        context_window: 1_048_576,
        max_tokens: 1_048_576,
    },
    FireworksModel {
        id: "accounts/fireworks/models/glm-5p3",
        name: "GLM 5.3",
        pricing: ModelPricing {
            base: ModelCost {
                input: 1.40,
                output: 4.40,
                cache_read: 0.26,
                cache_write: 0.0,
            },
            above: None,
        },
        context_window: 1_048_576,
        max_tokens: 1_048_576,
    },
    FireworksModel {
        id: "accounts/fireworks/models/kimi-k3",
        name: "Kimi K3",
        pricing: ModelPricing {
            base: ModelCost {
                input: 3.0,
                output: 15.0,
                cache_read: 0.30,
                cache_write: 0.0,
            },
            above: None,
        },
        context_window: 1_048_576,
        max_tokens: 1_048_576,
    },
    FireworksModel {
        id: "accounts/fireworks/models/deepseek-v4-pro-0813",
        name: "DeepSeek V4 Pro 0813",
        pricing: ModelPricing {
            base: ModelCost {
                input: 1.32,
                output: 3.96,
                cache_read: 0.044,
                cache_write: 0.0,
            },
            above: None,
        },
        context_window: 1_048_576,
        max_tokens: 393_216,
    },
    FireworksModel {
        id: "accounts/fireworks/models/deepseek-v4-flash-0731",
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
        context_window: 1_048_576,
        max_tokens: 393_216,
    },
    FireworksModel {
        id: "accounts/fireworks/models/qwen3p8-2p4t-a95b",
        name: "Qwen3.8 2.4T A95B",
        pricing: ModelPricing {
            base: ModelCost {
                input: 2.0,
                output: 6.0,
                cache_read: 0.25,
                cache_write: 0.0,
            },
            above: None,
        },
        context_window: 262_144,
        max_tokens: 262_144,
    },
];
