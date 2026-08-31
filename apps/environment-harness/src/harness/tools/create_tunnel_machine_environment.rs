use execution_contracts::MachineId;
use execution_protocol::{ConnectorKind, CreateEnvironmentRequest};
use llm_contracts::{ToolArguments, ToolDefinition};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use super::{
    ToolExecutionContext, ToolExecutionError, ToolOutput, argument_object, function_tool,
    json_output,
};

pub(super) fn definition() -> ToolDefinition {
    function_tool(
        "create_tunnel_machine_environment",
        "Create an environment on a tunneled machine. Returns the created environment details.",
        json!({
            "type": "object",
            "properties": {
                "name": {
                    "type": "string",
                    "description": "Display name for the environment"
                },
                "machine_id": {
                    "type": "string",
                    "description": "Tunneled machine ID returned by get_tunnel_machines_list"
                },
                "path": {
                    "type": "string",
                    "description": "Workspace-root-relative path for the environment"
                }
            },
            "required": ["name", "machine_id", "path"],
            "additionalProperties": false
        }),
    )
}

pub(super) async fn execute(
    arguments: &ToolArguments,
    context: &ToolExecutionContext<'_>,
) -> Result<ToolOutput, ToolExecutionError> {
    let arguments = serde_json::from_value::<CreateTunnelMachineEnvironmentArguments>(
        Value::Object(argument_object(arguments)?),
    )
    .map_err(|error| {
        ToolExecutionError::invalid_arguments(format!(
            "Invalid arguments for create_tunnel_machine_environment: {error}"
        ))
    })?;
    let machine_id = MachineId::new(arguments.machine_id).map_err(|error| {
        ToolExecutionError::invalid_arguments(format!(
            "Invalid arguments for create_tunnel_machine_environment: invalid `machine_id`: {error}"
        ))
    })?;
    let machine = context
        .execution
        .machine_summary(&machine_id)
        .await
        .map_err(|error| {
            ToolExecutionError::gateway(
                "machine_resolution_failed",
                &format!("Could not resolve machine `{machine_id}`"),
                error,
            )
        })?;
    if machine.connector != ConnectorKind::MachineDaemon {
        return Err(ToolExecutionError::new(
            "invalid_machine_type",
            format!("Machine `{machine_id}` is not a tunneled machine"),
        ));
    }
    let workspace_root_id = match machine.descriptor.workspace_roots.as_slice() {
        [root] => root.id.clone(),
        [] => {
            return Err(ToolExecutionError::new(
                "workspace_root_not_found",
                format!("Machine `{machine_id}` does not expose a workspace root"),
            ));
        }
        roots => {
            return Err(ToolExecutionError::new(
                "ambiguous_workspace_root",
                format!(
                    "Machine `{machine_id}` exposes {} workspace roots; expected exactly one",
                    roots.len()
                ),
            ));
        }
    };
    let environment = context
        .execution
        .create_environment(&CreateEnvironmentRequest {
            project_id: context.project_id,
            machine_id,
            name: arguments.name,
            workspace_root_id,
            path: arguments.path,
        })
        .await
        .map_err(|error| {
            ToolExecutionError::gateway(
                "environment_creation_failed",
                "Could not create tunnel-machine environment",
                error,
            )
        })?;

    output(CreateTunnelMachineEnvironmentResult::Successful {
        env_id: environment.environment_id.into_inner(),
        name: environment.name,
        path: environment.path,
        host_name: machine.name,
        created_at: environment.created_at.0,
    })
}

fn output(result: CreateTunnelMachineEnvironmentResult) -> Result<ToolOutput, ToolExecutionError> {
    json_output(
        result,
        "create_tunnel_machine_environment_result_serialization_failed",
        "tunnel machine environment creation result",
    )
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct CreateTunnelMachineEnvironmentArguments {
    name: String,
    machine_id: String,
    path: String,
}

#[derive(Serialize)]
#[serde(tag = "status", rename_all = "lowercase")]
enum CreateTunnelMachineEnvironmentResult {
    Successful {
        #[serde(rename = "envId")]
        env_id: String,
        name: String,
        path: String,
        #[serde(rename = "hostName")]
        host_name: String,
        #[serde(rename = "createdAt")]
        created_at: u64,
    },
}
