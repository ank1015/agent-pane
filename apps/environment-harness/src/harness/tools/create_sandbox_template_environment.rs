use execution_protocol::CreateSandboxTemplateEnvironmentRequest;
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
        "create_sandbox_template_environment",
        "Create a reusable sandbox template environment from a snapshot. Returns the created environment details.",
        json!({
            "type": "object",
            "properties": {
                "snapshot_id": {
                    "type": "string",
                    "description": "Snapshot ID to use as the template base"
                },
                "name": {
                    "type": "string",
                    "description": "Display name for the sandbox template environment"
                },
                "path": {
                    "type": "string",
                    "description": "Working directory relative to the sandbox workspace root"
                },
                "setup_script": {
                    "type": "string",
                    "description": "Optional shell script to run when materializing the environment"
                }
            },
            "required": ["snapshot_id", "name", "path"],
            "additionalProperties": false
        }),
    )
}

pub(super) async fn execute(
    arguments: &ToolArguments,
    context: &ToolExecutionContext<'_>,
) -> Result<ToolOutput, ToolExecutionError> {
    let arguments = serde_json::from_value::<CreateSandboxTemplateEnvironmentArguments>(
        Value::Object(argument_object(arguments)?),
    )
    .map_err(|error| {
        ToolExecutionError::invalid_arguments(format!(
            "Invalid arguments for create_sandbox_template_environment: {error}"
        ))
    })?;
    let environment = context
        .execution
        .create_sandbox_template_environment(&CreateSandboxTemplateEnvironmentRequest {
            project_id: context.project_id,
            snapshot_id: arguments.snapshot_id,
            name: arguments.name,
            path: arguments.path,
            creation_script: arguments.setup_script.unwrap_or_default(),
        })
        .await
        .map_err(|error| {
            ToolExecutionError::gateway(
                "environment_creation_failed",
                "Could not create sandbox template environment",
                error,
            )
        })?;
    let Some(snapshot_id) = environment.snapshot_id else {
        return Err(ToolExecutionError::new(
            "invalid_gateway_response",
            "The created sandbox template did not include its snapshot ID",
        ));
    };

    output(CreateSandboxTemplateEnvironmentResult::Successful {
        env_id: environment.id,
        name: environment.name,
        path: environment.path,
        host_name: environment.host_name,
        created_at: environment.created_at.0,
        snapshot_id,
    })
}

fn output(
    result: CreateSandboxTemplateEnvironmentResult,
) -> Result<ToolOutput, ToolExecutionError> {
    json_output(
        result,
        "create_sandbox_template_environment_result_serialization_failed",
        "sandbox template environment creation result",
    )
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct CreateSandboxTemplateEnvironmentArguments {
    snapshot_id: Uuid,
    name: String,
    path: String,
    #[serde(default)]
    setup_script: Option<String>,
}

#[derive(Serialize)]
#[serde(tag = "status", rename_all = "lowercase")]
enum CreateSandboxTemplateEnvironmentResult {
    Successful {
        #[serde(rename = "envId")]
        env_id: String,
        name: String,
        path: String,
        #[serde(rename = "hostName")]
        host_name: String,
        #[serde(rename = "createdAt")]
        created_at: u64,
        #[serde(rename = "snapshotId")]
        snapshot_id: Uuid,
    },
}
