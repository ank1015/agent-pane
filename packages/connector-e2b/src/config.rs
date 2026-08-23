use execution_contracts::{EnvironmentId, GrantId, MachineId, WorkspaceRoot, WorkspaceRootId};
use url::Url;

#[derive(Clone, Debug)]
pub struct E2bConnectionConfig {
    /// Stable envd proxy URL, normally `https://sandbox.e2b.app`.
    pub sandbox_url: Url,
    pub sandbox_id: String,
    pub envd_access_token: String,
    pub envd_port: u16,
    pub username: String,
    pub request_timeout_ms: u64,
    pub python_command: String,
}

impl E2bConnectionConfig {
    pub fn production(sandbox_id: impl Into<String>, envd_access_token: impl Into<String>) -> Self {
        Self {
            sandbox_url: Url::parse("https://sandbox.e2b.app")
                .expect("the E2B production sandbox URL is valid"),
            sandbox_id: sandbox_id.into(),
            envd_access_token: envd_access_token.into(),
            envd_port: 49_983,
            username: "user".to_owned(),
            request_timeout_ms: 60_000,
            python_command: "python3".to_owned(),
        }
    }
}

#[derive(Clone, Debug)]
pub struct E2bWorkspaceRoot {
    pub id: WorkspaceRootId,
    pub name: String,
    pub path: String,
    pub read_only: bool,
}

impl E2bWorkspaceRoot {
    pub(crate) fn contract(&self) -> WorkspaceRoot {
        WorkspaceRoot {
            id: self.id.clone(),
            name: self.name.clone(),
            uri: format!("file://{}", self.path),
            read_only: self.read_only,
        }
    }
}

#[derive(Clone, Debug)]
pub struct E2bNativeGrant {
    pub id: GrantId,
    /// Absolute sandbox path beneath which the grant is valid.
    pub path: String,
    pub read_only: bool,
}

#[derive(Clone, Debug)]
pub struct E2bEnvironmentConfig {
    pub machine_id: MachineId,
    pub environment_id: EnvironmentId,
    pub name: String,
    /// Absolute target-side directory used for prepared mutations, artifacts,
    /// idempotency records, and recoverable process journals.
    pub state_directory: String,
    pub workspace_roots: Vec<E2bWorkspaceRoot>,
    pub native_grants: Vec<E2bNativeGrant>,
}
