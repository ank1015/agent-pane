//! Catalog-backed, non-streaming Anthropic Messages transport.

mod client;
mod config;
mod error;
mod models;
mod request;
mod response;

pub use client::AnthropicProvider;
pub use config::{ANTHROPIC_API_VERSION, AnthropicConfig, DEFAULT_ANTHROPIC_TIMEOUT};
pub use models::{ANTHROPIC_MODELS, AnthropicModel, calculate_usage_cost, find_model};
pub use request::{
    ANTHROPIC_NATIVE_INPUT_TAG, DEFAULT_ANTHROPIC_MAX_TOKENS, build_message_request,
};
pub use response::convert_response;

/// Provider identifier used by this package.
pub const ANTHROPIC_PROVIDER: &str = "anthropic";

/// Default base URL for the Anthropic API.
pub const DEFAULT_ANTHROPIC_BASE_URL: &str = "https://api.anthropic.com/v1";
