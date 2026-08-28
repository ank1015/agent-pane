use llm_contracts::{ModelCost, ModelPricing, Usage, UsageCost};

/// Fireworks model supported by this provider package.
#[derive(Clone, Copy, Debug, PartialEq)]
#[non_exhaustive]
pub struct FireworksModel {
    pub id: &'static str,
    pub name: &'static str,
    pub pricing: ModelPricing,
    pub context_window: u64,
    pub max_tokens: u64,
}

include!("models.generated.rs");

/// Finds an allowed model by its exact provider-owned identifier.
#[must_use]
pub fn find_model(model_id: &str) -> Option<&'static FireworksModel> {
    FIREWORKS_MODELS.iter().find(|model| model.id == model_id)
}

/// Maps a portable reasoning level to the closest effort supported by a Fireworks model.
///
/// Multiple portable levels intentionally map to one native effort where the model exposes a
/// smaller reasoning scale.
#[must_use]
pub fn reasoning_effort(model_id: &str, reasoning_level: &str) -> Option<&'static str> {
    match (model_id, reasoning_level) {
        ("accounts/fireworks/models/kimi-k3", "low") => Some("low"),
        ("accounts/fireworks/models/kimi-k3", "medium") => Some("medium"),
        ("accounts/fireworks/models/kimi-k3", "high") => Some("high"),
        ("accounts/fireworks/models/kimi-k3", "xhigh" | "max") => Some("max"),

        ("accounts/fireworks/models/deepseek-v4-pro-0813", "low" | "medium" | "high") => {
            Some("high")
        }
        ("accounts/fireworks/models/deepseek-v4-pro-0813", "xhigh" | "max") => Some("max"),

        ("accounts/fireworks/models/qwen3p8-2p4t-a95b", "low") => Some("low"),
        ("accounts/fireworks/models/qwen3p8-2p4t-a95b", "medium") => Some("medium"),
        ("accounts/fireworks/models/qwen3p8-2p4t-a95b", "high" | "xhigh" | "max") => Some("high"),

        ("accounts/fireworks/models/deepseek-v4-flash-0731", "low") => Some("low"),
        ("accounts/fireworks/models/deepseek-v4-flash-0731", "medium" | "high") => Some("high"),
        ("accounts/fireworks/models/deepseek-v4-flash-0731", "xhigh" | "max") => Some("max"),
        _ => None,
    }
}

/// Calculates standard-tier USD cost from normalized Fireworks usage.
#[must_use]
pub fn calculate_usage_cost(usage: &Usage, model: &FireworksModel) -> UsageCost {
    let rates = model.pricing.base;
    let input = cost_for_tokens(usage.input.unwrap_or(0), rates.input);
    let output = cost_for_tokens(usage.output.unwrap_or(0), rates.output);
    let cache_read = cost_for_tokens(usage.cache_read.unwrap_or(0), rates.cache_read);
    UsageCost {
        input: Some(input),
        output: Some(output),
        cache_read: Some(cache_read),
        cache_write: None,
        total: input + output + cache_read,
    }
}

fn cost_for_tokens(tokens: u64, cost_per_million: f64) -> f64 {
    tokens as f64 * cost_per_million / 1_000_000.0
}
