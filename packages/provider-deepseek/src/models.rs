use llm_contracts::{ModelCost, ModelPricing, Usage, UsageCost};

/// DeepSeek model accepted by this provider package.
#[derive(Clone, Copy, Debug, PartialEq)]
#[non_exhaustive]
pub struct DeepSeekModel {
    pub id: &'static str,
    pub name: &'static str,
    /// Off-peak pricing. `pricing.above` is unused for these models.
    pub pricing: ModelPricing,
    pub peak_pricing: ModelCost,
    pub context_window: u64,
    pub max_tokens: u64,
    pub supports_images: bool,
}

include!("models.generated.rs");

/// Finds an allowed model by its exact DeepSeek identifier.
#[must_use]
pub fn find_model(model_id: &str) -> Option<&'static DeepSeekModel> {
    DEEPSEEK_MODELS.iter().find(|model| model.id == model_id)
}

/// Maps a portable reasoning level to the closest DeepSeek V4 effort.
///
/// DeepSeek documents one common effort scale for its V4 Chat Completions models.
#[must_use]
pub fn reasoning_effort(model_id: &str, reasoning_level: &str) -> Option<&'static str> {
    find_model(model_id)?;
    match reasoning_level {
        "low" => Some("low"),
        "medium" | "high" | "xhigh" => Some("high"),
        "max" => Some("max"),
        _ => None,
    }
}

/// Returns whether a Unix millisecond timestamp falls in DeepSeek peak pricing.
///
/// Peak windows are 01:00–04:00 and 06:00–10:00 UTC on weekdays, with end times exclusive.
#[must_use]
pub fn is_peak_pricing(timestamp_ms: u64) -> bool {
    const DAY_MS: u64 = 24 * 60 * 60 * 1_000;
    const HOUR_MS: u64 = 60 * 60 * 1_000;
    const UNIX_EPOCH_WEEKDAY_FROM_MONDAY: u64 = 3;
    let weekday_from_monday = (timestamp_ms / DAY_MS + UNIX_EPOCH_WEEKDAY_FROM_MONDAY) % 7;
    if weekday_from_monday >= 5 {
        return false;
    }
    let hour = (timestamp_ms % DAY_MS) / HOUR_MS;
    (1..4).contains(&hour) || (6..10).contains(&hour)
}

/// Calculates USD cost using the pricing tier active at request start.
#[must_use]
pub fn calculate_usage_cost(
    usage: &Usage,
    model: &DeepSeekModel,
    request_started_at_ms: u64,
) -> UsageCost {
    let rates = if is_peak_pricing(request_started_at_ms) {
        model.peak_pricing
    } else {
        model.pricing.base
    };
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
