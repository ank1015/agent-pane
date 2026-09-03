//! Provider-neutral contracts for LLM provider implementations.
//!
//! Completion and optional provider APIs have separate transport traits so a
//! provider only implements the operations it actually supports.

pub mod identifiers;
pub mod llm;
pub mod messages;
pub mod models;
pub mod validation;

pub use identifiers::{MessageId, ModelId, ProviderId, ToolCallId};
pub use llm::*;
pub use messages::*;
pub use models::*;
pub use validation::{Validate, ValidationError, ValidationIssue};

/// JSON object used for provider options, metadata, schemas, and native data.
pub type JsonObject = serde_json::Map<String, serde_json::Value>;
