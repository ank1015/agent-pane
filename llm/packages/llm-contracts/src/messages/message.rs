use serde::{Deserialize, Serialize};

use crate::{
    AssistantContent, JsonObject, MessageId, ModelRef, StopReason, ToolCallId, Usage, Validate,
    ValidationError,
    validation::{finish, issue, require_non_empty},
};

use super::{ContentPart, TextContent, validate_content};

/// Unix timestamp in milliseconds.
#[derive(Clone, Copy, Debug, Deserialize, Eq, Ord, PartialEq, PartialOrd, Serialize)]
#[serde(transparent)]
pub struct Timestamp(pub u64);

/// Message sent by a user.
#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct UserMessage {
    pub id: MessageId,
    pub timestamp: Timestamp,
    pub content: Vec<ContentPart>,
}

/// Operator-level text instructions inserted into a conversation.
#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct SystemMessage {
    pub id: MessageId,
    pub timestamp: Timestamp,
    pub content: Vec<TextContent>,
}

/// Structured error returned by a tool.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ToolResultError {
    pub message: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
}

/// Outcome of executing a tool call.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(tag = "status", rename_all = "snake_case")]
pub enum ToolResultOutcome {
    Success,
    Error { error: ToolResultError },
}

/// Result returned to the model after executing a tool.
#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ToolResultMessage {
    pub id: MessageId,
    pub tool_name: String,
    pub tool_call_id: ToolCallId,
    pub content: Vec<ContentPart>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub details: Option<serde_json::Value>,
    pub timestamp: Timestamp,
    pub outcome: ToolResultOutcome,
}

/// Complete assistant response produced by a provider.
#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct AssistantMessage {
    pub id: MessageId,
    pub model: ModelRef,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub usage: Option<Usage>,
    /// End-to-end request duration in milliseconds.
    pub duration_ms: u64,
    /// Unmodified provider response, retained for lossless provider replay.
    pub native_message: serde_json::Value,
    pub content: Vec<AssistantContent>,
    pub stop_reason: StopReason,
    pub timestamp: Timestamp,
}

/// Application-owned or provider-native message stored alongside LLM messages.
#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct CustomMessage {
    pub id: MessageId,
    pub content: JsonObject,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tag: Option<String>,
    pub timestamp: Timestamp,
}

/// A conversation message.
#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
#[serde(tag = "role", rename_all = "snake_case")]
pub enum Message {
    User(UserMessage),
    System(SystemMessage),
    ToolResult(ToolResultMessage),
    Assistant(AssistantMessage),
    Custom(CustomMessage),
}

impl Validate for UserMessage {
    fn validate(&self) -> Result<(), ValidationError> {
        validate_content(&self.content, "user.content")
    }
}

impl Validate for SystemMessage {
    fn validate(&self) -> Result<(), ValidationError> {
        if self.content.is_empty() {
            return Err(ValidationError::single(
                "system.content",
                "must contain at least one item",
            ));
        }
        Ok(())
    }
}

impl Validate for ToolResultMessage {
    fn validate(&self) -> Result<(), ValidationError> {
        let mut issues = Vec::new();
        require_non_empty(&mut issues, "tool_result.tool_name", &self.tool_name);
        if let Err(error) = validate_content(&self.content, "tool_result.content") {
            issues.extend(error.issues);
        }
        finish(issues)
    }
}

impl Validate for AssistantMessage {
    fn validate(&self) -> Result<(), ValidationError> {
        let mut issues = Vec::new();
        if let Err(error) = self.model.validate() {
            issues.extend(error.issues);
        }
        if let Some(usage) = &self.usage {
            if let Err(error) = usage.validate() {
                issues.extend(error.issues);
            }
        }
        for (index, content) in self.content.iter().enumerate() {
            if let Err(error) = content.validate() {
                for nested in error.issues {
                    issue(
                        &mut issues,
                        format!("assistant.content[{index}].{}", nested.path),
                        nested.message,
                    );
                }
            }
        }
        finish(issues)
    }
}

impl Validate for CustomMessage {
    fn validate(&self) -> Result<(), ValidationError> {
        if self.tag.as_ref().is_some_and(|tag| tag.trim().is_empty()) {
            return Err(ValidationError::single(
                "custom.tag",
                "must not be empty when set",
            ));
        }
        Ok(())
    }
}

impl Validate for Message {
    fn validate(&self) -> Result<(), ValidationError> {
        match self {
            Self::User(message) => message.validate(),
            Self::System(message) => message.validate(),
            Self::ToolResult(message) => message.validate(),
            Self::Assistant(message) => message.validate(),
            Self::Custom(message) => message.validate(),
        }
    }
}
