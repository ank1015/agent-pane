use llm_contracts::{ModelCost, ModelPricing, Usage, UsageCost};

/// OpenRouter model accepted by this provider package.
#[derive(Clone, Copy, Debug, PartialEq)]
#[non_exhaustive]
pub struct OpenRouterModel {
    pub id: &'static str,
    pub name: &'static str,
    pub canonical_slug: &'static str,
    pub pricing: ModelPricing,
    pub context_window: u64,
    pub max_tokens: u64,
    pub supported_parameters: &'static [&'static str],
}

include!("models.generated.rs");

/// Finds an allowed model by its exact OpenRouter identifier.
#[must_use]
pub fn find_model(model_id: &str) -> Option<&'static OpenRouterModel> {
    OPENROUTER_MODELS.iter().find(|model| model.id == model_id)
}

/// Calculates a fallback USD estimate from normalized usage and catalog rates.
///
/// Successful OpenRouter responses normally contain authoritative cost. This
/// helper is used only when the router omits it, and is also useful to callers.
#[must_use]
pub fn calculate_usage_cost(usage: &Usage, model: &OpenRouterModel) -> UsageCost {
    let rates = model.pricing.base;
    let input = cost_for_tokens(usage.input.unwrap_or(0), rates.input);
    let output = cost_for_tokens(usage.output.unwrap_or(0), rates.output);
    let cache_read = cost_for_tokens(usage.cache_read.unwrap_or(0), rates.cache_read);
    let cache_write = cost_for_tokens(usage.cache_write.unwrap_or(0), rates.cache_write);
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
