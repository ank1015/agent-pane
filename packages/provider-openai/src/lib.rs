//! Catalog-backed OpenAI provider support.

mod config;
mod error;
mod models;

pub use config::{DEFAULT_OPENAI_TIMEOUT, OpenAiConfig};
pub use models::{
    OPENAI_MODELS, OpenAiModel, calculate_usage_cost, find_model, select_model_pricing,
};

/// Provider identifier used by this package.
pub const OPENAI_PROVIDER: &str = "openai";

/// Default base URL for the OpenAI API.
pub const DEFAULT_OPENAI_BASE_URL: &str = "https://api.openai.com/v1";
