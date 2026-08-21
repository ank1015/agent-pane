use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use thiserror::Error;

use crate::{
    JsonObject, ToolCallId,
    messages::TextContent,
    validation::{Validate, ValidationError, ValidationIssue, issue, require_non_empty},
};

/// Why a provider stopped producing an assistant response.
#[derive(Clone, Copy, Debug, Deserialize, Eq, JsonSchema, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum StopReason {
    Stop,
    Length,
    ToolUse,
    Refusal,
    ContentFilter,
    PauseTurn,
}

/// Tool-call arguments emitted either as a parsed JSON object or raw text.
#[derive(Clone, Debug, Deserialize, JsonSchema, PartialEq, Serialize)]
#[serde(untagged)]
pub enum ToolArguments {
    Object(JsonObject),
    String(String),
}

/// A complete part of an assistant response.
#[derive(Clone, Debug, Deserialize, JsonSchema, PartialEq, Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum AssistantContent {
    Response {
        response: TextContent,
    },
    Thinking {
        thinking_text: String,
    },
    ToolCall {
        name: String,
        arguments: ToolArguments,
        tool_call_id: ToolCallId,
    },
}

impl Validate for AssistantContent {
    fn validate(&self) -> Result<(), ValidationError> {
        let mut issues = Vec::new();
        if let Self::ToolCall { name, .. } = self {
            require_non_empty(&mut issues, "tool_call.name", name);
        }
        finish(issues)
    }
}

/// Provider or transport failure that prevented an assistant response.
#[derive(Clone, Debug, Deserialize, Error, JsonSchema, PartialEq, Serialize)]
#[error("{message}")]
#[serde(deny_unknown_fields)]
pub struct LlmError {
    pub message: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub provider_code: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub provider_type: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub http_status: Option<u16>,
    pub can_retry: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub retry_after_ms: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub native_error: Option<serde_json::Value>,
}

impl Validate for LlmError {
    fn validate(&self) -> Result<(), ValidationError> {
        let mut issues = Vec::new();
        require_non_empty(&mut issues, "error.message", &self.message);
        if self
            .http_status
            .is_some_and(|status| !(100..=599).contains(&status))
        {
            issue(
                &mut issues,
                "error.http_status",
                "must be between 100 and 599",
            );
        }
        finish(issues)
    }
}

fn finish(issues: Vec<ValidationIssue>) -> Result<(), ValidationError> {
    ValidationError::from_issues(issues).map_or(Ok(()), Err)
}
