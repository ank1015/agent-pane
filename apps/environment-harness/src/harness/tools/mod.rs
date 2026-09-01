use std::time::{SystemTime, UNIX_EPOCH};

use execution_contracts::{MachineId, WorkspaceRootId};
use execution_gateway_client::ExecutionGatewayClientError;
use execution_runtime::{ExecutionRuntime, OperationContext};
use llm_contracts::{
    AssistantContent, ContentPart, FunctionTool, MessageId, TextContent, Timestamp, ToolArguments,
    ToolDefinition, ToolResultError, ToolResultMessage, ToolResultOutcome,
};
use serde::Serialize;
use serde_json::{Map, Value, json};
use uuid::Uuid;

use crate::clients::ExecutionClient;

mod create_sandbox;
mod create_sandbox_template_environment;
mod create_tunnel_machine_environment;
mod get_sandbox_accounts_list;
mod get_tunnel_machines_list;
mod list_environments;
mod snapshot_sandbox;
mod update_environment;

use create_sandbox::{definition as create_sandbox_definition, execute as execute_create_sandbox};
use create_sandbox_template_environment::{
    definition as create_sandbox_template_environment_definition,
    execute as execute_create_sandbox_template_environment,
};
use create_tunnel_machine_environment::{
    definition as create_tunnel_machine_environment_definition,
    execute as execute_create_tunnel_machine_environment,
};
use get_sandbox_accounts_list::{
    definition as get_sandbox_accounts_list_definition,
    execute as execute_get_sandbox_accounts_list,
};
use get_tunnel_machines_list::{
    definition as get_tunnel_machines_list_definition, execute as execute_get_tunnel_machines_list,
};
use list_environments::{
    definition as list_environments_definition, execute as execute_list_environments,
};
use snapshot_sandbox::{
    definition as snapshot_sandbox_definition, execute as execute_snapshot_sandbox,
};
use update_environment::{
    definition as update_environment_definition, execute as execute_update_environment,
};

pub struct ToolExecutionContext<'a> {
    pub execution: &'a ExecutionClient,
    pub operation: &'a OperationContext,
    pub project_id: Uuid,
    pub search: &'a tool_firecrawl_search::FirecrawlSearchToolContext,
    pub scrape: &'a tool_firecrawl_scrape::FirecrawlScrapeToolContext,
}

#[derive(Debug, thiserror::Error)]
#[error("{message}")]
pub struct ToolExecutionError {
    name: &'static str,
    message: String,
    details: Option<Value>,
}

impl ToolExecutionError {
    fn new(name: &'static str, message: impl Into<String>) -> Self {
        Self {
            name,
            message: message.into(),
            details: None,
        }
    }

    fn invalid_arguments(message: impl Into<String>) -> Self {
        Self::new("invalid_arguments", message)
    }

    fn gateway(name: &'static str, prefix: &str, error: ExecutionGatewayClientError) -> Self {
        let reason = match error {
            ExecutionGatewayClientError::Rejected {
                error: Some(error), ..
            } => error.message,
            error => error.to_string(),
        };
        Self::new(name, format!("{prefix}: {reason}"))
    }

    fn into_parts(self) -> (&'static str, String, Option<Value>) {
        (self.name, self.message, self.details)
    }
}

impl From<tool_pi_bash::BashToolError> for ToolExecutionError {
    fn from(error: tool_pi_bash::BashToolError) -> Self {
        let (name, message, details) = error.into_parts();
        Self {
            name,
            message,
            details,
        }
    }
}

impl From<tool_pi_edit::EditToolError> for ToolExecutionError {
    fn from(error: tool_pi_edit::EditToolError) -> Self {
        let (name, message, details) = error.into_parts();
        Self {
            name,
            message,
            details,
        }
    }
}

impl From<tool_pi_read::ReadToolError> for ToolExecutionError {
    fn from(error: tool_pi_read::ReadToolError) -> Self {
        let (name, message, details) = error.into_parts();
        Self {
            name,
            message,
            details,
        }
    }
}

impl From<tool_pi_write::WriteToolError> for ToolExecutionError {
    fn from(error: tool_pi_write::WriteToolError) -> Self {
        let (name, message, details) = error.into_parts();
        Self {
            name,
            message,
            details,
        }
    }
}

impl From<tool_firecrawl_search::FirecrawlSearchToolError> for ToolExecutionError {
    fn from(error: tool_firecrawl_search::FirecrawlSearchToolError) -> Self {
        let (name, message, details) = error.into_parts();
        Self {
            name,
            message,
            details,
        }
    }
}

impl From<tool_firecrawl_scrape::FirecrawlScrapeToolError> for ToolExecutionError {
    fn from(error: tool_firecrawl_scrape::FirecrawlScrapeToolError) -> Self {
        let (name, message, details) = error.into_parts();
        Self {
            name,
            message,
            details,
        }
    }
}

pub struct ToolOutput {
    pub content: Vec<ContentPart>,
    pub details: Option<Value>,
}

impl From<tool_pi_bash::BashToolOutput> for ToolOutput {
    fn from(output: tool_pi_bash::BashToolOutput) -> Self {
        Self {
            content: output.content,
            details: output.details,
        }
    }
}

impl From<tool_pi_edit::EditToolOutput> for ToolOutput {
    fn from(output: tool_pi_edit::EditToolOutput) -> Self {
        Self {
            content: output.content,
            details: output.details,
        }
    }
}

impl From<tool_pi_read::ReadToolOutput> for ToolOutput {
    fn from(output: tool_pi_read::ReadToolOutput) -> Self {
        Self {
            content: output.content,
            details: output.details,
        }
    }
}

impl From<tool_pi_write::WriteToolOutput> for ToolOutput {
    fn from(output: tool_pi_write::WriteToolOutput) -> Self {
        Self {
            content: output.content,
            details: output.details,
        }
    }
}

impl From<tool_firecrawl_search::FirecrawlSearchToolOutput> for ToolOutput {
    fn from(output: tool_firecrawl_search::FirecrawlSearchToolOutput) -> Self {
        Self {
            content: output.content,
            details: output.details,
        }
    }
}

impl From<tool_firecrawl_scrape::FirecrawlScrapeToolOutput> for ToolOutput {
    fn from(output: tool_firecrawl_scrape::FirecrawlScrapeToolOutput) -> Self {
        Self {
            content: output.content,
            details: output.details,
        }
    }
}

pub fn default_tool_definitions() -> Vec<ToolDefinition> {
    vec![
        get_tunnel_machines_list_definition(),
        get_sandbox_accounts_list_definition(),
        list_environments_definition(),
        create_sandbox_definition(),
        snapshot_sandbox_definition(),
        create_tunnel_machine_environment_definition(),
        create_sandbox_template_environment_definition(),
        update_environment_definition(),
        read_definition(),
        bash_definition(),
        edit_definition(),
        write_definition(),
        tool_firecrawl_search::definition(),
        tool_firecrawl_scrape::definition(),
    ]
}

pub async fn execute_tool_call(
    call: &AssistantContent,
    context: &ToolExecutionContext<'_>,
) -> Result<ToolResultMessage, ToolExecutionError> {
    let AssistantContent::ToolCall {
        name,
        tool_call_id,
        arguments,
    } = call
    else {
        return Err(ToolExecutionError::new(
            "invalid_tool_call",
            "assistant content is not a tool call",
        ));
    };
    let result = match name.as_str() {
        "get_tunnel_machines_list" => {
            execute_get_tunnel_machines_list(arguments, context.execution).await
        }
        "get_sandbox_accounts_list" => {
            execute_get_sandbox_accounts_list(arguments, context.execution).await
        }
        "list_environments" => execute_list_environments(arguments, context).await,
        "create_sandbox" => execute_create_sandbox(arguments, context.execution).await,
        "snapshot_sandbox" => execute_snapshot_sandbox(arguments, context.execution).await,
        "create_tunnel_machine_environment" => {
            execute_create_tunnel_machine_environment(arguments, context).await
        }
        "create_sandbox_template_environment" => {
            execute_create_sandbox_template_environment(arguments, context).await
        }
        "update_environment" => execute_update_environment(arguments, context).await,
        tool_firecrawl_search::TOOL_NAME => {
            tool_firecrawl_search::execute_search_tool(arguments, context.search)
                .await
                .map(Into::into)
                .map_err(Into::into)
        }
        tool_firecrawl_scrape::TOOL_NAME => {
            tool_firecrawl_scrape::execute_scrape_tool(arguments, context.scrape)
                .await
                .map(Into::into)
                .map_err(Into::into)
        }
        _ => match targeted_arguments(name, arguments) {
            Ok((machine_id, arguments)) => match context.execution.machine(&machine_id).await {
                Ok(machine) => {
                    execute_on_runtime(name, &arguments, &machine, context.operation).await
                }
                Err(error) => Err(ToolExecutionError::new(
                    "machine_resolution_failed",
                    format!("Could not resolve machine `{machine_id}`: {error}"),
                )),
            },
            Err(error) => Err(error),
        },
    };
    Ok(tool_result(name.clone(), tool_call_id.clone(), result))
}

/// Executes an environment tool against an already resolved runtime.
///
/// This is useful for conformance tests and preserves the same machine identity
/// and workspace-root checks used after gateway resolution in production.
pub async fn execute_tool_call_with_runtime(
    call: &AssistantContent,
    runtime: &dyn ExecutionRuntime,
    operation: &OperationContext,
) -> Result<ToolResultMessage, ToolExecutionError> {
    let AssistantContent::ToolCall {
        name,
        tool_call_id,
        arguments,
    } = call
    else {
        return Err(ToolExecutionError::new(
            "invalid_tool_call",
            "assistant content is not a tool call",
        ));
    };
    let result = match targeted_arguments(name, arguments) {
        Ok((machine_id, arguments)) if runtime.descriptor().machine_id == machine_id => {
            execute_on_runtime(name, &arguments, runtime, operation).await
        }
        Ok((machine_id, _)) => Err(ToolExecutionError::new(
            "machine_mismatch",
            format!(
                "Resolved machine `{}` does not match requested machine `{machine_id}`",
                runtime.descriptor().machine_id
            ),
        )),
        Err(error) => Err(error),
    };
    Ok(tool_result(name.clone(), tool_call_id.clone(), result))
}

fn targeted_arguments(
    name: &str,
    arguments: &ToolArguments,
) -> Result<(MachineId, ToolArguments), ToolExecutionError> {
    let mut arguments = argument_object(arguments)?;
    let machine_id = arguments.remove("machineId").ok_or_else(|| {
        ToolExecutionError::invalid_arguments(format!(
            "Invalid arguments for {name}: missing required field `machineId`"
        ))
    })?;
    let machine_id = machine_id.as_str().ok_or_else(|| {
        ToolExecutionError::invalid_arguments(format!(
            "Invalid arguments for {name}: `machineId` must be a string"
        ))
    })?;
    let machine_id = MachineId::new(machine_id).map_err(|error| {
        ToolExecutionError::invalid_arguments(format!(
            "Invalid arguments for {name}: invalid `machineId`: {error}"
        ))
    })?;
    Ok((machine_id, ToolArguments::Object(arguments)))
}

fn argument_object(arguments: &ToolArguments) -> Result<Map<String, Value>, ToolExecutionError> {
    match arguments {
        ToolArguments::Object(arguments) => Ok(arguments.clone()),
        ToolArguments::String(arguments) => {
            let value: Value = serde_json::from_str(arguments).map_err(|error| {
                ToolExecutionError::invalid_arguments(format!(
                    "Tool arguments must be a JSON object: {error}"
                ))
            })?;
            value.as_object().cloned().ok_or_else(|| {
                ToolExecutionError::invalid_arguments("Tool arguments must be a JSON object")
            })
        }
    }
}

fn require_no_arguments(name: &str, arguments: &ToolArguments) -> Result<(), ToolExecutionError> {
    let arguments = argument_object(arguments)?;
    if !arguments.is_empty() {
        return Err(ToolExecutionError::invalid_arguments(format!(
            "Invalid arguments for {name}: this tool does not accept arguments"
        )));
    }
    Ok(())
}

fn json_output<T: Serialize>(
    value: T,
    error_name: &'static str,
    subject: &str,
) -> Result<ToolOutput, ToolExecutionError> {
    let details = serde_json::to_value(value).map_err(|error| {
        ToolExecutionError::new(
            error_name,
            format!("Could not serialize {subject}: {error}"),
        )
    })?;
    let content = serde_json::to_string_pretty(&details).map_err(|error| {
        ToolExecutionError::new(
            error_name,
            format!("Could not serialize {subject}: {error}"),
        )
    })?;
    Ok(ToolOutput {
        content: vec![text_content(content)],
        details: Some(details),
    })
}

async fn execute_on_runtime(
    name: &str,
    arguments: &ToolArguments,
    runtime: &dyn ExecutionRuntime,
    operation: &OperationContext,
) -> Result<ToolOutput, ToolExecutionError> {
    let root_id = sole_workspace_root(runtime)?;
    match name {
        "bash" => execute_bash_tool(arguments, runtime, operation, root_id).await,
        "read" => execute_read_tool(arguments, runtime, operation, root_id).await,
        "write" => execute_write_tool(arguments, runtime, operation, root_id).await,
        "edit" => execute_edit_tool(arguments, runtime, operation, root_id).await,
        _ => Err(ToolExecutionError::new(
            "unknown_tool",
            format!("Unknown tool `{name}`"),
        )),
    }
}

fn sole_workspace_root(
    runtime: &dyn ExecutionRuntime,
) -> Result<WorkspaceRootId, ToolExecutionError> {
    match runtime.descriptor().workspace_roots.as_slice() {
        [root] => Ok(root.id.clone()),
        [] => Err(ToolExecutionError::new(
            "workspace_root_not_found",
            format!(
                "Machine `{}` does not expose a workspace root",
                runtime.descriptor().machine_id
            ),
        )),
        roots => Err(ToolExecutionError::new(
            "ambiguous_workspace_root",
            format!(
                "Machine `{}` exposes {} workspace roots; the tool call cannot choose one from `machineId` alone",
                runtime.descriptor().machine_id,
                roots.len()
            ),
        )),
    }
}

async fn execute_bash_tool(
    arguments: &ToolArguments,
    runtime: &dyn ExecutionRuntime,
    operation: &OperationContext,
    root_id: WorkspaceRootId,
) -> Result<ToolOutput, ToolExecutionError> {
    let context = tool_pi_bash::BashToolContext::new(runtime, operation, root_id, ".")?;
    tool_pi_bash::execute_bash_tool(arguments, &context)
        .await
        .map(Into::into)
        .map_err(Into::into)
}

async fn execute_edit_tool(
    arguments: &ToolArguments,
    runtime: &dyn ExecutionRuntime,
    operation: &OperationContext,
    root_id: WorkspaceRootId,
) -> Result<ToolOutput, ToolExecutionError> {
    let context = tool_pi_edit::EditToolContext::new(runtime, operation, root_id, ".")?;
    tool_pi_edit::execute_edit_tool(arguments, &context)
        .await
        .map(Into::into)
        .map_err(Into::into)
}

async fn execute_read_tool(
    arguments: &ToolArguments,
    runtime: &dyn ExecutionRuntime,
    operation: &OperationContext,
    root_id: WorkspaceRootId,
) -> Result<ToolOutput, ToolExecutionError> {
    let context = tool_pi_read::ReadToolContext::new(runtime, operation, root_id, ".")?;
    tool_pi_read::execute_read_tool(arguments, &context)
        .await
        .map(Into::into)
        .map_err(Into::into)
}

async fn execute_write_tool(
    arguments: &ToolArguments,
    runtime: &dyn ExecutionRuntime,
    operation: &OperationContext,
    root_id: WorkspaceRootId,
) -> Result<ToolOutput, ToolExecutionError> {
    let context = tool_pi_write::WriteToolContext::new(runtime, operation, root_id, ".")?;
    tool_pi_write::execute_write_tool(arguments, &context)
        .await
        .map(Into::into)
        .map_err(Into::into)
}

fn tool_result(
    name: String,
    tool_call_id: llm_contracts::ToolCallId,
    result: Result<ToolOutput, ToolExecutionError>,
) -> ToolResultMessage {
    let (content, details, outcome) = match result {
        Ok(output) => (output.content, output.details, ToolResultOutcome::Success),
        Err(error) => {
            let (error_name, message, details) = error.into_parts();
            (
                vec![text_content(message.clone())],
                details,
                ToolResultOutcome::Error {
                    error: ToolResultError {
                        message,
                        name: Some(error_name.to_owned()),
                    },
                },
            )
        }
    };
    ToolResultMessage {
        id: MessageId::new(format!("tool-result-{}", Uuid::now_v7()))
            .expect("UUID tool result identifier is valid"),
        tool_name: name,
        tool_call_id,
        content,
        details,
        timestamp: Timestamp(now_ms()),
        outcome,
    }
}

fn read_definition() -> ToolDefinition {
    function_tool(
        "read",
        "Read a text file or image from a machine's workspace root. Text output is limited to 2000 lines or 50KB; use offset and limit to continue large files.",
        json!({
            "type": "object",
            "properties": {
                "machineId": machine_id_schema(),
                "path": {"type": "string", "description": "Path relative to the machine's workspace root"},
                "offset": {"type": "number", "description": "Line number to start reading from (1-indexed)"},
                "limit": {"type": "number", "description": "Maximum number of lines to read"}
            },
            "required": ["machineId", "path"],
            "additionalProperties": false
        }),
    )
}

fn write_definition() -> ToolDefinition {
    function_tool(
        "write",
        "Create or overwrite a file in a machine's workspace root, creating parent directories when needed.",
        json!({
            "type": "object",
            "properties": {
                "machineId": machine_id_schema(),
                "path": {"type": "string", "description": "Path relative to the machine's workspace root"},
                "content": {"type": "string", "description": "Content to write to the file"}
            },
            "required": ["machineId", "path", "content"],
            "additionalProperties": false
        }),
    )
}

fn edit_definition() -> ToolDefinition {
    function_tool(
        "edit",
        "Edit one file in a machine's workspace root using unique, non-overlapping exact text replacements matched against the original file.",
        json!({
            "type": "object",
            "properties": {
                "machineId": machine_id_schema(),
                "path": {"type": "string", "description": "Path relative to the machine's workspace root"},
                "edits": {
                    "type": "array",
                    "description": "One or more exact replacements matched against the original file",
                    "items": {
                        "type": "object",
                        "properties": {
                            "oldText": {"type": "string", "description": "Unique exact text to replace"},
                            "newText": {"type": "string", "description": "Replacement text"}
                        },
                        "required": ["oldText", "newText"],
                        "additionalProperties": false
                    }
                }
            },
            "required": ["machineId", "path", "edits"],
            "additionalProperties": false
        }),
    )
}

fn bash_definition() -> ToolDefinition {
    function_tool(
        "bash",
        "Execute a Bash command from a machine's workspace root. Output is limited to the last 2000 lines or 50KB, with full truncated output retained as an artifact.",
        json!({
            "type": "object",
            "properties": {
                "machineId": machine_id_schema(),
                "command": {"type": "string", "description": "Bash command to execute"},
                "timeout": {"type": "number", "description": "Optional timeout in seconds"}
            },
            "required": ["machineId", "command"],
            "additionalProperties": false
        }),
    )
}

fn machine_id_schema() -> Value {
    json!({
        "type": "string",
        "minLength": 1,
        "description": "ID of the execution machine whose workspace root should be used"
    })
}

fn function_tool(name: &str, description: &str, parameters: Value) -> ToolDefinition {
    let Value::Object(parameters) = parameters else {
        unreachable!("tool parameters are declared as an object")
    };
    ToolDefinition::Function(FunctionTool {
        name: name.to_owned(),
        description: description.to_owned(),
        parameters,
        output_schema: None,
        strict: None,
    })
}

fn text_content(content: impl Into<String>) -> ContentPart {
    ContentPart::Text(TextContent {
        content: content.into(),
        metadata: None,
    })
}

fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .ok()
        .and_then(|duration| u64::try_from(duration.as_millis()).ok())
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use llm_contracts::{ToolDefinition, Validate};
    use serde_json::json;

    use super::default_tool_definitions;

    #[test]
    fn default_definitions_are_valid_and_ordered() {
        let tools = default_tool_definitions();
        let names: Vec<_> = tools.iter().map(|tool| tool.name()).collect();

        assert_eq!(
            names,
            [
                "get_tunnel_machines_list",
                "get_sandbox_accounts_list",
                "list_environments",
                "create_sandbox",
                "snapshot_sandbox",
                "create_tunnel_machine_environment",
                "create_sandbox_template_environment",
                "update_environment",
                "read",
                "bash",
                "edit",
                "write",
                "search",
                "scrape"
            ]
        );
        for tool in tools {
            tool.validate().expect("default tool definition is valid");
            let ToolDefinition::Function(tool) = tool else {
                panic!("environment tools are functions")
            };
            if matches!(
                tool.name.as_str(),
                "get_tunnel_machines_list" | "get_sandbox_accounts_list" | "list_environments"
            ) {
                assert_eq!(tool.parameters["properties"], json!({}));
                assert_eq!(tool.parameters["required"], json!([]));
                continue;
            }
            if tool.name == "create_sandbox" {
                assert_eq!(
                    tool.parameters["properties"]["account_id"]["type"],
                    "string"
                );
                assert!(
                    tool.parameters["required"]
                        .as_array()
                        .expect("required array")
                        .iter()
                        .any(|field| field == "account_id")
                );
                continue;
            }
            if tool.name == "snapshot_sandbox" {
                assert_eq!(
                    tool.parameters["properties"]["machine_id"]["type"],
                    "string"
                );
                assert!(
                    tool.parameters["required"]
                        .as_array()
                        .expect("required array")
                        .iter()
                        .any(|field| field == "machine_id")
                );
                continue;
            }
            if tool.name == "create_tunnel_machine_environment" {
                assert_eq!(tool.parameters["properties"]["name"]["type"], "string");
                assert_eq!(
                    tool.parameters["properties"]["machine_id"]["type"],
                    "string"
                );
                assert_eq!(tool.parameters["properties"]["path"]["type"], "string");
                assert_eq!(
                    tool.parameters["required"],
                    json!(["name", "machine_id", "path"])
                );
                continue;
            }
            if tool.name == "create_sandbox_template_environment" {
                assert_eq!(
                    tool.parameters["properties"]["snapshot_id"]["type"],
                    "string"
                );
                assert_eq!(tool.parameters["properties"]["name"]["type"], "string");
                assert_eq!(tool.parameters["properties"]["path"]["type"], "string");
                assert_eq!(
                    tool.parameters["properties"]["setup_script"]["type"],
                    "string"
                );
                assert_eq!(
                    tool.parameters["required"],
                    json!(["snapshot_id", "name", "path"])
                );
                continue;
            }
            if tool.name == "update_environment" {
                assert_eq!(
                    tool.parameters["properties"]["environment_id"]["type"],
                    "string"
                );
                assert_eq!(tool.parameters["required"], json!(["environment_id"]));
                continue;
            }
            if tool.name == "search" {
                assert_eq!(tool.parameters["required"], json!(["query"]));
                assert!(tool.parameters["properties"].get("machineId").is_none());
                continue;
            }
            if tool.name == "scrape" {
                assert_eq!(tool.parameters["required"], json!(["url"]));
                assert!(tool.parameters["properties"].get("machineId").is_none());
                continue;
            }
            assert_eq!(tool.parameters["properties"]["machineId"]["type"], "string");
            assert!(
                tool.parameters["required"]
                    .as_array()
                    .expect("required array")
                    .iter()
                    .any(|field| field == "machineId")
            );
        }
    }
}
