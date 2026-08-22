use llm_contracts::{ModelCost, ModelPricing, Usage, UsageCost};

/// Anthropic model supported by this provider package.
#[derive(Clone, Copy, Debug, PartialEq)]
#[non_exhaustive]
pub struct AnthropicModel {
    pub id: &'static str,
    pub name: &'static str,
    pub pricing: ModelPricing,
    /// Per-million-token price for one-hour cache creation.
    pub cache_write_1h: f64,
    pub context_window: u64,
    pub max_tokens: u64,
}

include!("models.generated.rs");

/// Finds an allowed model by its exact provider-owned identifier.
#[must_use]
pub fn find_model(model_id: &str) -> Option<&'static AnthropicModel> {
    ANTHROPIC_MODELS.iter().find(|model| model.id == model_id)
}

/// Calculates USD cost using the default five-minute cache-write rate.
///
/// Response conversion uses Anthropic's native TTL breakdown when present so
/// one-hour cache creation is priced at its distinct rate.
#[must_use]
pub fn calculate_usage_cost(usage: &Usage, model: &AnthropicModel) -> UsageCost {
    calculate_usage_cost_with_cache_ttl(usage, model, usage.cache_write.unwrap_or(0), 0)
}

pub(crate) fn calculate_usage_cost_with_cache_ttl(
    usage: &Usage,
    model: &AnthropicModel,
    cache_write_5m: u64,
    cache_write_1h: u64,
) -> UsageCost {
    let rates = model.pricing.base;
    let input = cost_for_tokens(usage.input.unwrap_or(0), rates.input);
    let output = cost_for_tokens(usage.output.unwrap_or(0), rates.output);
    let cache_read = cost_for_tokens(usage.cache_read.unwrap_or(0), rates.cache_read);
    let cache_write = cost_for_tokens(cache_write_5m, rates.cache_write)
        + cost_for_tokens(cache_write_1h, model.cache_write_1h);
    UsageCost {
        input: Some(input),
        output: Some(output),
        cache_read: Some(cache_read),
        cache_write: Some(cache_write),
        total: input + output + cache_read + cache_write,
    }
}

fn cost_for_tokens(tokens: u64, cost_per_million: f64) -> f64 {
    tokens as f64 * cost_per_million / 1_000_000.0
}
