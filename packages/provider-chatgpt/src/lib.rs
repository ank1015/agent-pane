//! Catalog-backed ChatGPT backend support.

mod config;
mod error;
mod models;

pub use config::{ChatGptConfig, DEFAULT_CHATGPT_TIMEOUT};
pub use models::{CHATGPT_MODELS, ChatGptModel, find_model};

/// Provider identifier used by this package.
pub const CHATGPT_PROVIDER: &str = "chatgpt";

/// Default ChatGPT backend base URL.
pub const DEFAULT_CHATGPT_BASE_URL: &str = "https://chatgpt.com/backend-api";

/// Default instructions required when the portable request does not supply any.
pub const DEFAULT_CHATGPT_INSTRUCTIONS: &str = "You are a helpful assistant.";
