use llm_contracts::{ModelCost, ModelPricing, Usage, UsageCost};

/// OpenAI model supported by this provider package.
#[derive(Clone, Copy, Debug, PartialEq)]
#[non_exhaustive]
pub struct OpenAiModel {
    pub id: &'static str,
    pub name: &'static str,
    pub pricing: ModelPricing,
    pub context_window: u64,
    pub max_tokens: u64,
}

include!("models.generated.rs");

/// Finds an allowed model by its exact provider-owned identifier.
#[must_use]
pub fn find_model(model_id: &str) -> Option<&'static OpenAiModel> {
    OPENAI_MODELS.iter().find(|model| model.id == model_id)
}

/// Selects base or long-context prices using total prompt tokens.
#[must_use]
pub fn select_model_pricing(model: &OpenAiModel, prompt_tokens: u64) -> ModelCost {
    match model.pricing.above {
        Some(above) if prompt_tokens > above.prompt_tokens => above.cost,
        _ => model.pricing.base,
    }
}

/// Calculates the complete per-bucket USD cost for a normalized usage record.
#[must_use]
pub fn calculate_usage_cost(usage: &Usage, model: &OpenAiModel, prompt_tokens: u64) -> UsageCost {
    let rates = select_model_pricing(model, prompt_tokens);
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
