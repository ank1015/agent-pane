use std::collections::BTreeMap;

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::{
    JsonObject,
    messages::Message,
    models::ModelRef,
    validation::{Validate, ValidationError, ValidationIssue, issue, require_non_empty},
};

use super::ToolDefinition;

/// Serializable, provider-neutral model request.
#[derive(Clone, Debug, Deserialize, JsonSchema, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct LlmRequest {
    pub model: ModelRef,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub instructions: Option<String>,
    #[serde(default)]
    pub messages: Vec<Message>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub tools: Vec<ToolDefinition>,
    #[serde(default, skip_serializing_if = "JsonObject::is_empty")]
    pub provider_options: JsonObject,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub metadata: BTreeMap<String, String>,
}

impl Validate for LlmRequest {
    fn validate(&self) -> Result<(), ValidationError> {
        let mut issues = Vec::new();

        if let Err(error) = self.model.validate() {
            issues.extend(error.issues);
        }
        if let Some(instructions) = &self.instructions {
            require_non_empty(&mut issues, "request.instructions", instructions);
        }
        for (index, message) in self.messages.iter().enumerate() {
            append_nested(
                &mut issues,
                format!("request.messages[{index}]"),
                message.validate(),
            );
        }
        for (index, tool) in self.tools.iter().enumerate() {
            append_nested(
                &mut issues,
                format!("request.tools[{index}]"),
                tool.validate(),
            );
        }

        let mut names = std::collections::HashSet::new();
        for tool in &self.tools {
            if !names.insert(tool.name()) {
                issue(
                    &mut issues,
                    "request.tools",
                    format!("tool name `{}` is duplicated", tool.name()),
                );
            }
        }

        finish(issues)
    }
}

fn append_nested(
    issues: &mut Vec<ValidationIssue>,
    parent: String,
    result: Result<(), ValidationError>,
) {
    if let Err(error) = result {
        for nested in error.issues {
            issue(issues, format!("{parent}.{}", nested.path), nested.message);
        }
    }
}

fn finish(issues: Vec<ValidationIssue>) -> Result<(), ValidationError> {
    ValidationError::from_issues(issues).map_or(Ok(()), Err)
}
