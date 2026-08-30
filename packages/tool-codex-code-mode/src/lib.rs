//! Model-facing Codex `exec` and `wait` tools backed by
//! [`codex_code_mode_runtime`].
//!
//! This crate owns definition formation, JavaScript pragma parsing, nested
//! tool metadata, result truncation, and transcript-facing output. The harness
//! owns the session and the delegate that executes nested tools.

mod description;
mod execution;
mod json_schema_types;
mod output;

pub use description::{
    CODE_MODE_PRAGMA_PREFIX, ImageDetailVisibility, ParsedExecSource, ToolNamespaceDescription,
    augment_tool_definition, build_exec_tool_description, build_wait_tool_description,
    is_code_mode_nested_tool, normalize_code_mode_identifier, parse_exec_source,
    render_code_mode_sample,
};
pub use execution::{
    CodeModeToolContext, WaitArguments, execute_exec, execute_exec_tool, execute_wait,
    execute_wait_tool, parse_exec_arguments, parse_wait_arguments,
};
pub use output::{CodeModeToolError, CodeModeToolOutput};

use std::collections::BTreeMap;

use codex_code_mode_runtime::{
    CodeModeToolKind, DEFAULT_EXEC_YIELD_TIME_MS, ToolDefinition as RuntimeToolDefinition, ToolName,
};
use llm_contracts::{CustomTool, CustomToolFormat, FunctionTool, GrammarSyntax, ToolDefinition};
use serde_json::{Value, json};

pub const EXEC_TOOL_NAME: &str = "exec";
/// Compatibility name used by Codex's description renderer.
pub const PUBLIC_TOOL_NAME: &str = EXEC_TOOL_NAME;
pub const WAIT_TOOL_NAME: &str = "wait";
pub const EXEC_LARK_GRAMMAR: &str = include_str!("exec.lark");

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct CodeModeDefinitionOptions {
    pub default_exec_yield_time_ms: u64,
    pub code_mode_only: bool,
    pub image_detail_visibility: ImageDetailVisibility,
}

impl Default for CodeModeDefinitionOptions {
    fn default() -> Self {
        Self {
            default_exec_yield_time_ms: DEFAULT_EXEC_YIELD_TIME_MS,
            code_mode_only: true,
            image_detail_visibility: ImageDetailVisibility::Visible,
        }
    }
}

/// Returns the model-facing custom `exec` definition.
#[must_use]
pub fn exec_definition(nested_tools: &[ToolDefinition]) -> ToolDefinition {
    exec_definition_with_options(nested_tools, CodeModeDefinitionOptions::default())
}

/// Returns `exec` with harness-selected description behavior.
#[must_use]
pub fn exec_definition_with_options(
    nested_tools: &[ToolDefinition],
    options: CodeModeDefinitionOptions,
) -> ToolDefinition {
    let prompt_tools = collect_prompt_tool_definitions(nested_tools);
    ToolDefinition::Custom(CustomTool {
        name: EXEC_TOOL_NAME.to_owned(),
        description: build_exec_tool_description(
            &prompt_tools,
            &[],
            &BTreeMap::new(),
            options.default_exec_yield_time_ms,
            options.code_mode_only,
            options.image_detail_visibility,
        ),
        format: CustomToolFormat {
            syntax: GrammarSyntax::Lark,
            definition: EXEC_LARK_GRAMMAR.to_owned(),
        },
    })
}

/// Returns Codex's model-facing `wait` function definition.
#[must_use]
pub fn wait_definition() -> ToolDefinition {
    let Value::Object(parameters) = json!({
        "type": "object",
        "properties": {
            "cell_id": {
                "type": "string",
                "description": "Identifier of the running exec cell."
            },
            "yield_time_ms": {
                "type": "number",
                "description": "Wait before yielding more output. Defaults to 10000 ms."
            },
            "max_tokens": {
                "type": "number",
                "description": "Output token budget for this wait call. Defaults to 10000 tokens."
            },
            "terminate": {
                "type": "boolean",
                "description": "True stops the running exec cell; false or omitted waits for output."
            }
        },
        "required": ["cell_id"],
        "additionalProperties": false
    }) else {
        unreachable!("wait parameters are an object")
    };
    ToolDefinition::Function(FunctionTool {
        name: WAIT_TOOL_NAME.to_owned(),
        description: format!(
            "Waits on a yielded `{EXEC_TOOL_NAME}` cell and returns new output or completion.\n{}",
            build_wait_tool_description().trim()
        ),
        parameters,
        output_schema: None,
        strict: Some(false),
    })
}

/// Returns `exec` and `wait` in model-facing order.
#[must_use]
pub fn definitions(nested_tools: &[ToolDefinition]) -> Vec<ToolDefinition> {
    vec![exec_definition(nested_tools), wait_definition()]
}

/// Converts portable definitions to the augmented metadata installed in V8.
#[must_use]
pub fn collect_runtime_tool_definitions(
    nested_tools: &[ToolDefinition],
) -> Vec<RuntimeToolDefinition> {
    collect_prompt_tool_definitions(nested_tools)
        .into_iter()
        .map(augment_tool_definition)
        .collect()
}

fn collect_prompt_tool_definitions(nested_tools: &[ToolDefinition]) -> Vec<RuntimeToolDefinition> {
    let mut definitions = nested_tools
        .iter()
        .filter(|tool| is_code_mode_nested_tool(tool.name()))
        .map(portable_tool_definition)
        .collect::<Vec<_>>();
    definitions.sort_by(|left, right| left.name.cmp(&right.name));
    definitions.dedup_by(|left, right| left.name == right.name);
    definitions
}

fn portable_tool_definition(tool: &ToolDefinition) -> RuntimeToolDefinition {
    match tool {
        ToolDefinition::Function(tool) => RuntimeToolDefinition {
            name: normalize_code_mode_identifier(&tool.name),
            tool_name: ToolName::plain(tool.name.clone()),
            description: tool.description.clone(),
            kind: CodeModeToolKind::Function,
            input_schema: Some(Value::Object(tool.parameters.clone())),
            output_schema: tool.output_schema.clone().map(Value::Object),
        },
        ToolDefinition::Custom(tool) => RuntimeToolDefinition {
            name: normalize_code_mode_identifier(&tool.name),
            tool_name: ToolName::plain(tool.name.clone()),
            description: tool.description.clone(),
            kind: CodeModeToolKind::Freeform,
            input_schema: None,
            output_schema: None,
        },
    }
}
