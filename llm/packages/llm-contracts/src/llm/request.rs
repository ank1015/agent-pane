use std::collections::{BTreeMap, HashSet};

use serde::{Deserialize, Serialize};

use crate::{
    JsonObject, Message, ModelRef, ToolDefinition, Validate, ValidationError,
    validation::{finish, issue},
};

/// Serializable, provider-neutral model completion request.
#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
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

        append_nested(&mut issues, "request.model", self.model.validate());
        if self
            .instructions
            .as_ref()
            .is_some_and(|instructions| instructions.trim().is_empty())
        {
            issue(
                &mut issues,
                "request.instructions",
                "must not be empty when set",
            );
        }
        for (index, message) in self.messages.iter().enumerate() {
            append_nested(
                &mut issues,
                &format!("request.messages[{index}]"),
                message.validate(),
            );
        }
        for (index, tool) in self.tools.iter().enumerate() {
            append_nested(
                &mut issues,
                &format!("request.tools[{index}]"),
                tool.validate(),
            );
        }

        let mut names = HashSet::new();
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
    issues: &mut Vec<crate::ValidationIssue>,
    parent: &str,
    result: Result<(), ValidationError>,
) {
    if let Err(error) = result {
        for nested in error.issues {
            issue(issues, format!("{parent}.{}", nested.path), nested.message);
        }
    }
}
