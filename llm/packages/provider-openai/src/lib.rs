//! Catalog-backed, non-streaming OpenAI Responses API transport.

mod client;
mod config;
mod error;
mod models;
mod request;
mod response;

pub use client::OpenAiProvider;
pub use config::{DEFAULT_OPENAI_TIMEOUT, OpenAiConfig};
pub use models::{
    OPENAI_MODELS, OpenAiModel, calculate_usage_cost, find_model, select_model_pricing,
};
pub use request::{
    CODEX_RESPONSES_LITE_OPTION, OPENAI_NATIVE_INPUT_TAG, build_response_request,
    uses_codex_responses_lite,
};
pub use response::convert_response;

/// Provider identifier used by this package.
pub const OPENAI_PROVIDER: &str = "openai";

/// Default base URL for the OpenAI API.
pub const DEFAULT_OPENAI_BASE_URL: &str = "https://api.openai.com/v1";
