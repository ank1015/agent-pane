// @generated from Anthropic's direct API catalog and pricing pages on 2026-08-22.

/// Complete allowlist of current Anthropic models accepted by this provider.
pub const ANTHROPIC_MODELS: &[AnthropicModel] = &[
    AnthropicModel {
        id: "claude-fable-5",
        name: "Claude Fable 5",
        pricing: ModelPricing {
            base: ModelCost {
                input: 10.0,
                output: 50.0,
                cache_read: 1.0,
                cache_write: 12.5,
            },
            above: None,
        },
        cache_write_1h: 20.0,
        context_window: 1_000_000,
        max_tokens: 128_000,
    },
    AnthropicModel {
        id: "claude-opus-5",
        name: "Claude Opus 5",
        pricing: ModelPricing {
            base: ModelCost {
                input: 5.0,
                output: 25.0,
                cache_read: 0.5,
                cache_write: 6.25,
            },
            above: None,
        },
        cache_write_1h: 10.0,
        context_window: 1_000_000,
        max_tokens: 128_000,
    },
    AnthropicModel {
        id: "claude-sonnet-5",
        name: "Claude Sonnet 5",
        pricing: ModelPricing {
            // Introductory direct-API pricing is effective through 2026-08-31.
            base: ModelCost {
                input: 2.0,
                output: 10.0,
                cache_read: 0.2,
                cache_write: 2.5,
            },
            above: None,
        },
        cache_write_1h: 4.0,
        context_window: 1_000_000,
        max_tokens: 128_000,
    },
];
