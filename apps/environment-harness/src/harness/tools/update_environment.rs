use execution_contracts::MachineId;
use execution_protocol::{
    ConnectorKind, ProjectEnvironment, ProjectEnvironmentType, UpdateEnvironmentRequest,
    UpdateSandboxTemplateEnvironmentRequest,
};
use llm_contracts::{ToolArguments, ToolDefinition};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use uuid::Uuid;

use super::{
    ToolExecutionContext, ToolExecutionError, ToolOutput, argument_object, function_tool,
    json_output,
};

pub(super) fn definition() -> ToolDefinition {
    function_tool(
        "update_environment",
        "Update an existing tunnel-machine or sandbox-template environment. Use list_environments first to obtain its ID and type. Only supply fields valid for that type.",
        json!({
            "type": "object",
            "properties": {
                "environment_id": {
                    "type": "string",
                    "description": "Environment ID returned by list_environments"
                },
                "name": {
                    "type": "string",
                    "description": "Optional new display name"
                },
                "path": {
                    "type": "string",
                    "description": "Optional new working directory relative to the workspace root"
                },
                "machine_id": {
                    "type": "string",
                    "description": "Optional new tunneled machine ID; valid only for tunnel-machine environments"
                },
                "snapshot_id": {
                    "type": "string",
                    "description": "Optional new snapshot ID; valid only for sandbox-template environments"
                },
                "setup_script": {
                    "type": "string",
                    "description": "Optional replacement materialization setup script; valid only for sandbox-template environments. Use an empty string to clear it."
                }
            },
            "required": ["environment_id"],
            "additionalProperties": false
        }),
    )
}

pub(super) async fn execute(
    arguments: &ToolArguments,
    context: &ToolExecutionContext<'_>,
) -> Result<ToolOutput, ToolExecutionError> {
    let arguments = serde_json::from_value::<UpdateEnvironmentArguments>(Value::Object(
        argument_object(arguments)?,
    ))
    .map_err(|error| {
        ToolExecutionError::invalid_arguments(format!(
            "Invalid arguments for update_environment: {error}"
        ))
    })?;
    if arguments.name.is_none()
        && arguments.path.is_none()
        && arguments.machine_id.is_none()
        && arguments.snapshot_id.is_none()
        && arguments.setup_script.is_none()
    {
        return Err(ToolExecutionError::invalid_arguments(
            "Invalid arguments for update_environment: supply at least one field to update",
        ));
    }

    let current = context
        .execution
        .project_environments(&context.project_id)
        .await
        .map_err(|error| {
            ToolExecutionError::gateway(
                "environment_list_failed",
                "Could not resolve the environment to update",
                error,
            )
        })?
        .into_iter()
        .find(|environment| environment.id == arguments.environment_id)
        .ok_or_else(|| {
            ToolExecutionError::new(
                "environment_not_found",
                format!("Environment `{}` was not found", arguments.environment_id),
            )
        })?;

    let updated = match current.environment_type {
        ProjectEnvironmentType::Env => update_tunnel(arguments, context).await?,
        ProjectEnvironmentType::Template => update_template(arguments, context).await?,
    };
    json_output(
        UpdateEnvironmentResult::Successful {
            environment: updated,
        },
        "environment_update_result_serialization_failed",
        "environment update result",
    )
}

async fn update_tunnel(
    arguments: UpdateEnvironmentArguments,
    context: &ToolExecutionContext<'_>,
) -> Result<ProjectEnvironment, ToolExecutionError> {
    if arguments.snapshot_id.is_some() || arguments.setup_script.is_some() {
        return Err(ToolExecutionError::invalid_arguments(
            "Invalid arguments for update_environment: snapshot_id and setup_script are only valid for sandbox-template environments",
        ));
    }
    let (machine_id, workspace_root_id) = match arguments.machine_id {
        Some(machine_id) => {
            let machine_id = MachineId::new(machine_id).map_err(|error| {
                ToolExecutionError::invalid_arguments(format!(
                    "Invalid arguments for update_environment: invalid `machine_id`: {error}"
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
            let root = match machine.descriptor.workspace_roots.as_slice() {
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
            (Some(machine_id), Some(root))
        }
        None => (None, None),
    };
    context
        .execution
        .update_environment(
            &arguments.environment_id,
            &UpdateEnvironmentRequest {
                project_id: context.project_id,
                machine_id,
                workspace_root_id,
                name: arguments.name,
                path: arguments.path,
            },
        )
        .await
        .map_err(|error| {
            ToolExecutionError::gateway(
                "environment_update_failed",
                "Could not update tunnel-machine environment",
                error,
            )
        })
}

async fn update_template(
    arguments: UpdateEnvironmentArguments,
    context: &ToolExecutionContext<'_>,
) -> Result<ProjectEnvironment, ToolExecutionError> {
    if arguments.machine_id.is_some() {
        return Err(ToolExecutionError::invalid_arguments(
            "Invalid arguments for update_environment: machine_id is only valid for tunnel-machine environments",
        ));
    }
    let template_id = Uuid::parse_str(&arguments.environment_id).map_err(|error| {
        ToolExecutionError::invalid_arguments(format!(
            "Invalid sandbox-template environment ID: {error}"
        ))
    })?;
    context
        .execution
        .update_sandbox_template_environment(
            &template_id.to_string(),
            &UpdateSandboxTemplateEnvironmentRequest {
                project_id: context.project_id,
                snapshot_id: arguments.snapshot_id,
                name: arguments.name,
                path: arguments.path,
                creation_script: arguments.setup_script,
            },
        )
        .await
        .map_err(|error| {
            ToolExecutionError::gateway(
                "environment_update_failed",
                "Could not update sandbox-template environment",
                error,
            )
        })
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct UpdateEnvironmentArguments {
    environment_id: String,
    #[serde(default)]
    name: Option<String>,
    #[serde(default)]
    path: Option<String>,
    #[serde(default)]
    machine_id: Option<String>,
    #[serde(default)]
    snapshot_id: Option<Uuid>,
    #[serde(default)]
    setup_script: Option<String>,
}

#[derive(Serialize)]
#[serde(tag = "status", rename_all = "lowercase")]
enum UpdateEnvironmentResult {
    Successful { environment: ProjectEnvironment },
}
