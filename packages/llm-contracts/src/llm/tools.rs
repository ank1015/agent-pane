use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::{
    JsonObject,
    validation::{Validate, ValidationError, ValidationIssue, require_non_empty},
};

/// JSON-Schema-backed function tool exposed to a model.
#[derive(Clone, Debug, Deserialize, JsonSchema, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct FunctionTool {
    pub name: String,
    pub description: String,
    pub parameters: JsonObject,
    /// Optional schema describing the value returned to code-mode or other
    /// harness-owned tool consumers. Providers may ignore this metadata.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub output_schema: Option<JsonObject>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub strict: Option<bool>,
}

impl FunctionTool {
    /// Creates a function tool and generates its parameter schema from a Rust type.
    pub fn for_type<T>(
        name: impl Into<String>,
        description: impl Into<String>,
    ) -> Result<Self, ValidationError>
    where
        T: JsonSchema,
    {
        let schema = schemars::schema_for!(T);
        let value = serde_json::to_value(schema).map_err(|error| {
            ValidationError::single(
                "tool.parameters",
                format!("failed to serialize JSON Schema: {error}"),
            )
        })?;
        let serde_json::Value::Object(parameters) = value else {
            return Err(ValidationError::single(
                "tool.parameters",
                "generated JSON Schema must be an object",
            ));
        };
        let tool = Self {
            name: name.into(),
            description: description.into(),
            parameters,
            output_schema: None,
            strict: None,
        };
        ToolDefinition::Function(tool.clone()).validate()?;
        Ok(tool)
    }
}

/// Grammar syntax supported by a custom tool.
#[derive(Clone, Copy, Debug, Deserialize, Eq, JsonSchema, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum GrammarSyntax {
    Lark,
}

/// Grammar-based output format for a custom tool.
#[derive(Clone, Debug, Deserialize, Eq, JsonSchema, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct CustomToolFormat {
    pub syntax: GrammarSyntax,
    pub definition: String,
}

/// Provider custom tool with a constrained output grammar.
#[derive(Clone, Debug, Deserialize, Eq, JsonSchema, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct CustomTool {
    pub name: String,
    pub description: String,
    pub format: CustomToolFormat,
}

/// Tool definition that can safely cross a process or network boundary.
#[derive(Clone, Debug, Deserialize, JsonSchema, PartialEq, Serialize)]
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

fn require_tool_fields(issues: &mut Vec<ValidationIssue>, name: &str, description: &str) {
    require_non_empty(issues, "tool.name", name);
    require_non_empty(issues, "tool.description", description);
}

fn finish(issues: Vec<ValidationIssue>) -> Result<(), ValidationError> {
    ValidationError::from_issues(issues).map_or(Ok(()), Err)
}
