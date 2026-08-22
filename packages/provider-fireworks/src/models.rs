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
