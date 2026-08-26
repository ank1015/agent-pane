use execution_protocol::{Environment, MachineSummary};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use uuid::Uuid;
use zeroize::{Zeroize, ZeroizeOnDrop};

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum SandboxProvider {
    E2b,
    Daytona,
    Blaxel,
    Tensorlake,
}

#[derive(Deserialize, Serialize, Zeroize, ZeroizeOnDrop)]
#[serde(deny_unknown_fields)]
pub struct CreateSandboxAccountRequest {
    #[zeroize(skip)]
    pub provider: SandboxProvider,
    pub name: String,
    pub api_key: String,
    #[zeroize(skip)]
    #[serde(default = "empty_object", skip_serializing_if = "is_empty_object")]
    pub config: Value,
    #[zeroize(skip)]
    #[serde(default = "default_true", skip_serializing_if = "is_true")]
    pub enabled: bool,
    #[zeroize(skip)]
    #[serde(default, skip_serializing_if = "is_false")]
    pub make_default: bool,
}

#[derive(Deserialize, Serialize, Zeroize, ZeroizeOnDrop)]
#[serde(deny_unknown_fields)]
pub struct RotateSandboxCredentialsRequest {
    pub api_key: String,
}

#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct UpdateNameRequest {
    pub name: String,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum CredentialValidationStatus {
    Unchecked,
    Valid,
    Invalid,
}

#[derive(Debug, Deserialize, Serialize)]
pub struct SandboxAccount {
    pub id: Uuid,
    pub provider: SandboxProvider,
    pub name: String,
    pub config: Value,
    pub enabled: bool,
    pub is_default: bool,
    pub validation_status: CredentialValidationStatus,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_validated_at: Option<u64>,
    pub credential_version: i64,
    pub credentials_updated_at: u64,
    pub created_at: u64,
    pub updated_at: u64,
}

#[derive(Debug, Deserialize, Serialize)]
pub struct MachineInventory {
    pub connector_accounts: Vec<SandboxAccount>,
    pub machine_daemons: Vec<MachineSummary>,
}

#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct CreateMachineEnvironmentRequest {
    pub name: String,
    pub workspace_root_id: String,
    pub path: String,
}

#[derive(Debug, Deserialize, Serialize)]
pub struct Snapshot {
    pub id: Uuid,
    pub sandbox_account_id: Uuid,
    pub provider: SandboxProvider,
    pub provider_snapshot_id: String,
    pub sandbox_id: String,
    pub created_at: u64,
}

#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct CreateSnapshotRequest {
    pub provider: SandboxProvider,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sandbox_account_id: Option<Uuid>,
    #[serde(alias = "snapshot_sandbox_id")]
    pub provider_snapshot_id: String,
    pub sandbox_id: String,
}

#[derive(Default, Deserialize, Serialize)]
pub struct SnapshotQuery {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub provider: Option<SandboxProvider>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sandbox_account_id: Option<Uuid>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sandbox_id: Option<String>,
}

#[derive(Debug, Deserialize, Serialize)]
pub struct SandboxEnvironmentTemplate {
    pub id: Uuid,
    pub name: String,
    pub snapshot_id: Uuid,
    pub sandbox_account_id: Uuid,
    pub provider: SandboxProvider,
    pub cwd: String,
    pub creation_script: String,
    pub created_at: u64,
    pub updated_at: u64,
}

#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct CreateSandboxEnvironmentTemplateRequest {
    pub name: String,
    pub snapshot_id: Uuid,
    pub cwd: String,
    #[serde(default)]
    pub creation_script: String,
}

#[derive(Default, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct UpdateSandboxEnvironmentTemplateRequest {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub snapshot_id: Option<Uuid>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cwd: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub creation_script: Option<String>,
}

#[derive(Debug, Deserialize, Serialize)]
pub struct SandboxEnvironmentInstance {
    pub template_id: Uuid,
    pub provider: SandboxProvider,
    pub provider_sandbox_id: String,
    pub environment: Environment,
    pub created_at: u64,
}

#[derive(Serialize)]
pub(crate) struct SandboxAccountResponse {
    pub account: SandboxAccount,
}

#[derive(Serialize)]
pub(crate) struct MachineResponse {
    pub machine: MachineSummary,
}

fn empty_object() -> Value {
    serde_json::json!({})
}

fn is_empty_object(value: &Value) -> bool {
    value.as_object().is_some_and(serde_json::Map::is_empty)
}

const fn default_true() -> bool {
    true
}

const fn is_true(value: &bool) -> bool {
    *value
}

const fn is_false(value: &bool) -> bool {
    !*value
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::{CreateSandboxAccountRequest, SandboxProvider};

    #[test]
    fn create_request_accepts_supported_sandbox_providers() {
        for provider in ["e2b", "daytona", "blaxel", "tensorlake"] {
            let request: CreateSandboxAccountRequest = serde_json::from_value(json!({
                "provider": provider,
                "name": "main",
                "api_key": "secret"
            }))
            .unwrap();

            assert_eq!(request.name, "main");
        }
    }

    #[test]
    fn create_request_rejects_non_sandbox_machine_types() {
        for provider in ["ssh", "machine_server"] {
            let result = serde_json::from_value::<CreateSandboxAccountRequest>(json!({
                "provider": provider,
                "name": "main",
                "api_key": "secret"
            }));

            assert!(result.is_err());
        }
    }

    #[test]
    fn provider_serializes_to_the_gateway_contract() {
        assert_eq!(
            serde_json::to_value(SandboxProvider::Tensorlake).unwrap(),
            json!("tensorlake")
        );
    }
}
