use execution_contracts::MachineId;
use llm_contracts::{ToolArguments, ToolDefinition};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use crate::clients::ExecutionClient;

use super::{ToolExecutionError, ToolOutput, argument_object, function_tool, json_output};

pub(super) fn definition() -> ToolDefinition {
    function_tool(
        "snapshot_sandbox",
        "Create a reusable snapshot of an existing sandbox machine. Returns the new snapshot ID.",
        json!({
            "type": "object",
            "properties": {
                "machine_id": {
                    "type": "string",
                    "description": "Machine ID of the sandbox to snapshot"
                }
            },
            "required": ["machine_id"],
            "additionalProperties": false
        }),
    )
}

pub(super) async fn execute(
    arguments: &ToolArguments,
    execution: &ExecutionClient,
) -> Result<ToolOutput, ToolExecutionError> {
    let arguments = serde_json::from_value::<SnapshotSandboxArguments>(Value::Object(
        argument_object(arguments)?,
    ))
    .map_err(|error| {
        ToolExecutionError::invalid_arguments(format!(
            "Invalid arguments for snapshot_sandbox: {error}"
        ))
    })?;
    let machine_id = MachineId::new(arguments.machine_id).map_err(|error| {
        ToolExecutionError::invalid_arguments(format!(
            "Invalid arguments for snapshot_sandbox: invalid `machine_id`: {error}"
        ))
    })?;

    execution
        .snapshot_sandbox(&machine_id)
        .await
        .map_err(|error| {
            ToolExecutionError::gateway(
                "sandbox_snapshot_failed",
                "Could not snapshot sandbox",
                error,
            )
        })
        .and_then(|created| {
            output(SnapshotSandboxResult::Successful {
                snapshot_id: created.snapshot_id,
            })
        })
}

fn output(result: SnapshotSandboxResult) -> Result<ToolOutput, ToolExecutionError> {
    json_output(
        result,
        "snapshot_sandbox_result_serialization_failed",
        "sandbox snapshot result",
    )
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct SnapshotSandboxArguments {
    machine_id: String,
}

#[derive(Serialize)]
#[serde(tag = "status", rename_all = "lowercase")]
enum SnapshotSandboxResult {
    Successful {
        #[serde(rename = "snapshotId")]
        snapshot_id: uuid::Uuid,
    },
}
