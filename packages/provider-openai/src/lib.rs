//! Catalog-backed OpenAI model support.

mod models;

pub use models::{
    OPENAI_MODELS, OpenAiModel, calculate_usage_cost, find_model, select_model_pricing,
};
