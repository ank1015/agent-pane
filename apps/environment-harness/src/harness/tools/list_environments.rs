use llm_contracts::{ToolArguments, ToolDefinition};
use serde_json::json;

use super::{
    ToolExecutionContext, ToolExecutionError, ToolOutput, function_tool, json_output,
    require_no_arguments,
};

pub(super) fn definition() -> ToolDefinition {
    function_tool(
        "list_environments",
        "List the configured tunnel-machine environments and sandbox template environments. Tunnel entries include machine_id; template entries include snapshot_id and setup_script.",
        json!({
            "type": "object",
            "properties": {},
            "required": [],
            "additionalProperties": false
        }),
    )
}

pub(super) async fn execute(
    arguments: &ToolArguments,
    context: &ToolExecutionContext<'_>,
) -> Result<ToolOutput, ToolExecutionError> {
    require_no_arguments("list_environments", arguments)?;
    let environments = context
        .execution
        .project_environments(&context.project_id)
        .await
        .map_err(|error| {
            ToolExecutionError::new(
                "environment_list_failed",
                format!("Could not list environments: {error}"),
            )
        })?;
    json_output(
        environments,
        "environment_list_serialization_failed",
        "environment list",
    )
}
