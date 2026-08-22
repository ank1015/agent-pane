//! Catalog-backed DeepSeek Chat Completions support.

mod config;
mod error;
mod models;

pub use config::{DEFAULT_DEEPSEEK_TIMEOUT, DeepSeekConfig};
pub use models::{
    DEEPSEEK_MODELS, DeepSeekModel, calculate_usage_cost, find_model, is_peak_pricing,
};

/// Provider identifier used by this package.
pub const DEEPSEEK_PROVIDER: &str = "deepseek";

/// Default base URL for DeepSeek's OpenAI-compatible API.
pub const DEFAULT_DEEPSEEK_BASE_URL: &str = "https://api.deepseek.com";
