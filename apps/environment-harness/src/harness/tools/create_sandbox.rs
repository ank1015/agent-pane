use llm_contracts::{ToolArguments, ToolDefinition};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use uuid::Uuid;

use crate::clients::ExecutionClient;

use super::{ToolExecutionError, ToolOutput, argument_object, function_tool, json_output};

pub(super) fn definition() -> ToolDefinition {
    function_tool(
        "create_sandbox",
        "Create a new sandbox for a configured account, optionally restoring it from a saved snapshot. Returns the created machine and workspace-root details.",
        json!({
            "type": "object",
            "properties": {
                "account_id": {
                    "type": "string",
                    "description": "Sandbox account ID returned by get_sandbox_accounts_list"
                },
                "snapshot_id": {
                    "type": "string",
                    "description": "Optional saved snapshot ID to create the sandbox from"
                }
            },
            "required": ["account_id"],
            "additionalProperties": false
        }),
    )
}

pub(super) async fn execute(
    arguments: &ToolArguments,
    execution: &ExecutionClient,
) -> Result<ToolOutput, ToolExecutionError> {
    let arguments = serde_json::from_value::<CreateSandboxArguments>(Value::Object(
        argument_object(arguments)?,
    ))
    .map_err(|error| {
        ToolExecutionError::invalid_arguments(format!(
            "Invalid arguments for create_sandbox: {error}"
        ))
    })?;

    let created = execution
        .create_sandbox(&arguments.account_id, arguments.snapshot_id)
        .await
        .map_err(|error| {
            ToolExecutionError::gateway(
                "sandbox_creation_failed",
                "Could not create sandbox",
                error,
            )
        })?;
    let roots = created.machine.descriptor.workspace_roots;
    let workspace_root = match roots.as_slice() {
        [root] => root.clone(),
        [] => {
            return Err(ToolExecutionError::new(
                "workspace_root_not_found",
                "The created sandbox did not expose a workspace root",
            ));
        }
        roots => {
            return Err(ToolExecutionError::new(
                "ambiguous_workspace_root",
                format!(
                    "The created sandbox exposed {} workspace roots; expected exactly one",
                    roots.len()
                ),
            ));
        }
    };
    output(CreateSandboxResult::Successful {
        machine_id: created.machine.machine_id.into_inner(),
        workspace_root: SandboxWorkspaceRoot {
            id: workspace_root.id.into_inner(),
            name: workspace_root.name,
            uri: workspace_root.uri,
            read_only: workspace_root.read_only,
        },
    })
}

fn output(result: CreateSandboxResult) -> Result<ToolOutput, ToolExecutionError> {
    json_output(
        result,
        "create_sandbox_result_serialization_failed",
        "sandbox creation result",
    )
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct CreateSandboxArguments {
    account_id: Uuid,
    #[serde(default)]
    snapshot_id: Option<Uuid>,
}

#[derive(Serialize)]
#[serde(tag = "status", rename_all = "lowercase")]
enum CreateSandboxResult {
    Successful {
        #[serde(rename = "machineId")]
        machine_id: String,
        #[serde(rename = "workspaceRoot")]
        workspace_root: SandboxWorkspaceRoot,
    },
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct SandboxWorkspaceRoot {
    id: String,
    name: String,
    uri: String,
    read_only: bool,
}
