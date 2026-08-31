use llm_contracts::{ToolArguments, ToolDefinition};
use serde::Serialize;
use serde_json::json;

use crate::clients::ExecutionClient;

use super::{ToolExecutionError, ToolOutput, function_tool, json_output, require_no_arguments};

pub(super) fn definition() -> ToolDefinition {
    function_tool(
        "get_sandbox_accounts_list",
        "List configured execution-gateway sandbox accounts. Returns a JSON array of objects containing accountName, providerName, and accountId.",
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
    require_no_arguments("get_sandbox_accounts_list", arguments)?;

    let accounts = execution.sandbox_accounts().await.map_err(|error| {
        ToolExecutionError::new(
            "sandbox_account_list_failed",
            format!("Could not list sandbox accounts: {error}"),
        )
    })?;
    let accounts = accounts
        .into_iter()
        .map(|account| SandboxAccount {
            account_name: account.account_name,
            provider_name: account.provider_name,
            account_id: account.account_id,
        })
        .collect::<Vec<_>>();
    json_output(
        accounts,
        "sandbox_account_list_serialization_failed",
        "sandbox account list",
    )
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct SandboxAccount {
    account_name: String,
    provider_name: String,
    account_id: String,
}
