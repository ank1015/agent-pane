//! ChatGPT Codex backend transport with a non-streaming public interface.

mod client;
mod config;
mod error;
mod models;
mod request;
mod response;

pub use client::ChatGptProvider;
pub use config::{ChatGptConfig, DEFAULT_CHATGPT_TIMEOUT};
pub use models::{CHATGPT_MODELS, ChatGptModel, find_model};
pub use request::{CHATGPT_NATIVE_INPUT_TAG, build_response_request};
pub use response::convert_response_events;

/// Provider identifier used by this package.
pub const CHATGPT_PROVIDER: &str = "chatgpt";

/// Default ChatGPT backend base URL.
pub const DEFAULT_CHATGPT_BASE_URL: &str = "https://chatgpt.com/backend-api";

/// Default instructions required when the portable request does not supply any.
pub const DEFAULT_CHATGPT_INSTRUCTIONS: &str = "You are a helpful assistant.";
