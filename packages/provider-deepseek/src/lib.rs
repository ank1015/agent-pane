//! Catalog-backed, non-streaming DeepSeek Chat Completions transport.

mod client;
mod config;
mod error;
mod models;
mod request;
mod response;

pub use client::DeepSeekProvider;
pub use config::{DEFAULT_DEEPSEEK_TIMEOUT, DeepSeekConfig};
pub use models::{
    DEEPSEEK_MODELS, DeepSeekModel, calculate_usage_cost, find_model, is_peak_pricing,
    reasoning_effort,
};
pub use request::{DEEPSEEK_NATIVE_INPUT_TAG, build_chat_completion_request};
pub use response::convert_response;

/// Provider identifier used by this package.
pub const DEEPSEEK_PROVIDER: &str = "deepseek";

/// Default base URL for DeepSeek's OpenAI-compatible API.
pub const DEFAULT_DEEPSEEK_BASE_URL: &str = "https://api.deepseek.com";
