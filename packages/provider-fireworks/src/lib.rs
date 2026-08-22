//! Catalog-backed Fireworks Chat Completions support.

mod config;
mod error;
mod models;

pub use config::{DEFAULT_FIREWORKS_TIMEOUT, FireworksConfig};
pub use models::{FIREWORKS_MODELS, FireworksModel, calculate_usage_cost, find_model};

/// Provider identifier used by this package.
pub const FIREWORKS_PROVIDER: &str = "fireworks";

/// Default base URL for the Fireworks inference API.
pub const DEFAULT_FIREWORKS_BASE_URL: &str = "https://api.fireworks.ai/inference/v1";
