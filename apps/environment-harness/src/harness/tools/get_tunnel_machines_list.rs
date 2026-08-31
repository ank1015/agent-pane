use execution_protocol::ConnectorKind;
use llm_contracts::{ToolArguments, ToolDefinition};
use serde::Serialize;
use serde_json::json;

use crate::clients::ExecutionClient;

use super::{ToolExecutionError, ToolOutput, function_tool, json_output, require_no_arguments};

pub(super) fn definition() -> ToolDefinition {
    function_tool(
        "get_tunnel_machines_list",
        "List connected, online tunneled execution machines and their workspace roots. Returns a JSON array of objects containing name, machineId, and workspaceRoots.",
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
    execution: &ExecutionClient,
) -> Result<ToolOutput, ToolExecutionError> {
    require_no_arguments("get_tunnel_machines_list", arguments)?;

    let machines = execution.machines().await.map_err(|error| {
        ToolExecutionError::new(
            "machine_list_failed",
            format!("Could not list tunneled machines: {error}"),
        )
    })?;
    let machines = machines
        .into_iter()
        .filter(|machine| machine.connector == ConnectorKind::MachineDaemon && machine.online)
        .map(|machine| TunnelMachine {
            name: machine.name,
            machine_id: machine.machine_id.into_inner(),
            workspace_roots: machine
                .descriptor
                .workspace_roots
                .into_iter()
                .map(|root| TunnelWorkspaceRoot {
                    id: root.id.into_inner(),
                    name: root.name,
                    uri: root.uri,
                    read_only: root.read_only,
                })
                .collect(),
        })
        .collect::<Vec<_>>();
    json_output(
        machines,
        "machine_list_serialization_failed",
        "tunneled machine list",
    )
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct TunnelMachine {
    name: String,
    machine_id: String,
    workspace_roots: Vec<TunnelWorkspaceRoot>,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct TunnelWorkspaceRoot {
    id: String,
    name: String,
    uri: String,
    read_only: bool,
}
