//! Provider-neutral data contracts for LLM applications.
//!
//! The crate keeps requests and messages serializable so the same types can be
//! used in processes, HTTP APIs, queues, and persistence layers.

pub mod identifiers;
pub mod models;
pub mod providers;
pub mod validation;

pub use identifiers::{MessageId, ModelId, ProviderId, ToolCallId};
pub use models::*;
pub use validation::{ContractError, Validate, ValidationError, ValidationIssue};

/// JSON object used for provider-specific options, metadata, and native data.
pub type JsonObject = serde_json::Map<String, serde_json::Value>;
