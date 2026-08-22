//! Curated-catalog, non-streaming OpenRouter Chat Completions transport.

mod client;
mod config;
mod error;
mod models;
mod request;
mod response;

pub use client::OpenRouterProvider;
pub use config::{DEFAULT_OPENROUTER_TIMEOUT, OpenRouterConfig};
pub use models::{OPENROUTER_MODELS, OpenRouterModel, calculate_usage_cost, find_model};
pub use request::{OPENROUTER_NATIVE_INPUT_TAG, build_chat_completion_request};
pub use response::convert_response;

/// Provider identifier used by this package.
pub const OPENROUTER_PROVIDER: &str = "openrouter";

/// Default base URL for the OpenRouter API.
pub const DEFAULT_OPENROUTER_BASE_URL: &str = "https://openrouter.ai/api/v1";
