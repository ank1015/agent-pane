use execution_contracts::{
    ApplyMutationRequest, ContentSource, MutationAtomicity, MutationOperation, MutationPlan,
    MutationPostActions,
};
use llm_contracts::{ToolArguments, ToolDefinition};
use serde::Deserialize;
use serde_json::json;

use super::{
    ToolExecutionContext, ToolExecutionError, ToolOutput, function_tool, operation_id,
    parse_arguments, resolve_path,
};

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct WriteArguments {
    path: String,
    content: String,
}

pub fn definition() -> ToolDefinition {
    function_tool(
        "write",
        "Write content to a file in the current workspace. Creates the file if it doesn't exist, overwrites if it does, and automatically creates parent directories.",
        json!({
            "type": "object",
            "properties": {
                "path": {
                    "type": "string",
                    "description": "Path to the file to write (relative or absolute within the current workspace)"
                },
                "content": {
                    "type": "string",
                    "description": "Content to write to the file"
                }
            },
            "required": ["path", "content"]
        }),
    )
}

pub async fn execute_write_tool(
    arguments: &ToolArguments,
    context: &ToolExecutionContext<'_>,
) -> Result<ToolOutput, ToolExecutionError> {
    let arguments: WriteArguments = parse_arguments("write", arguments)?;
    let path = resolve_path(context, &arguments.path)?;
    let mutation = context
        .environment
        .workspace_mutation()
        .ok_or_else(|| ToolExecutionError::missing_capability("workspace mutations"))?;
    let bytes = arguments.content.len();
    let result = mutation
        .apply(
            context.operation,
            ApplyMutationRequest {
                operation_id: operation_id("pi-write"),
                plan: MutationPlan {
                    operations: vec![MutationOperation::PutFile {
                        path,
                        content: ContentSource::Text {
                            content: arguments.content,
                        },
                        create_parents: true,
                        preserve_utf8_bom: false,
                        expected_revision: None,
                    }],
                    atomicity: MutationAtomicity::AtomicIfSupported,
                    post_actions: MutationPostActions::default(),
                },
            },
        )
        .await?;

    Ok(ToolOutput::text(format!(
        "Successfully wrote {bytes} bytes to {}",
        arguments.path
    ))
    .with_details(json!({ "mutation": result })))
}
