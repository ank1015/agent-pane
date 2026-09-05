use serde::{Deserialize, Serialize};

use crate::{
    JsonObject, Validate, ValidationError,
    validation::{finish, require_non_empty},
};

/// JSON-Schema-backed function tool exposed to a model.
#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct FunctionTool {
    pub name: String,
    pub description: String,
    pub parameters: JsonObject,
    /// Optional output schema for application-owned tool consumers.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub output_schema: Option<JsonObject>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub strict: Option<bool>,
}

/// Grammar syntax supported by a custom tool.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum GrammarSyntax {
    Lark,
}

/// Grammar-based output format for a custom tool.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct CustomToolFormat {
    pub syntax: GrammarSyntax,
    pub definition: String,
}

/// Provider custom tool with a constrained output grammar.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct CustomTool {
    pub name: String,
    pub description: String,
    pub format: CustomToolFormat,
}

/// Tool definition that can cross a process or network boundary.
#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ToolDefinition {
    Function(FunctionTool),
    Custom(CustomTool),
}

impl ToolDefinition {
    #[must_use]
    pub fn name(&self) -> &str {
        match self {
            Self::Function(tool) => &tool.name,
            Self::Custom(tool) => &tool.name,
        }
    }
}

impl Validate for ToolDefinition {
    fn validate(&self) -> Result<(), ValidationError> {
        let mut issues = Vec::new();
        match self {
            Self::Function(tool) => {
                require_tool_fields(&mut issues, &tool.name, &tool.description);
            }
            Self::Custom(tool) => {
                require_tool_fields(&mut issues, &tool.name, &tool.description);
                require_non_empty(
                    &mut issues,
                    "tool.format.definition",
                    &tool.format.definition,
                );
            }
        }
        finish(issues)
    }
}

fn require_tool_fields(issues: &mut Vec<crate::ValidationIssue>, name: &str, description: &str) {
    require_non_empty(issues, "tool.name", name);
    require_non_empty(issues, "tool.description", description);
}
