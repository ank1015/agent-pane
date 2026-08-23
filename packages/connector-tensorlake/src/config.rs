use execution_contracts::{EnvironmentId, GrantId, MachineId, WorkspaceRoot, WorkspaceRootId};
use url::Url;

#[derive(Clone)]
pub struct TensorlakeConnectionConfig {
    /// Sandbox ingress endpoint, such as
    /// `https://<sandbox-id-or-name>.sandbox.tensorlake.ai/`.
    pub proxy_url: Url,
    pub api_key: String,
    pub user: String,
    pub request_timeout_ms: u64,
    pub python_command: String,
}

impl std::fmt::Debug for TensorlakeConnectionConfig {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("TensorlakeConnectionConfig")
            .field("proxy_url", &self.proxy_url)
            .field("api_key", &"[REDACTED]")
            .field("user", &self.user)
            .field("request_timeout_ms", &self.request_timeout_ms)
            .field("python_command", &self.python_command)
            .finish()
    }
}

impl TensorlakeConnectionConfig {
    pub fn new(proxy_url: Url, api_key: impl Into<String>) -> Self {
        Self {
            proxy_url,
            api_key: api_key.into(),
            user: "tl-user".to_owned(),
            request_timeout_ms: 60_000,
            python_command: "python3".to_owned(),
        }
    }

    pub fn for_sandbox(
        identifier: &str,
        api_key: impl Into<String>,
    ) -> Result<Self, url::ParseError> {
        Url::parse(&format!("https://{identifier}.sandbox.tensorlake.ai/"))
            .map(|url| Self::new(url, api_key))
    }
}

#[derive(Clone, Debug)]
pub struct TensorlakeWorkspaceRoot {
    pub id: WorkspaceRootId,
    pub name: String,
    pub path: String,
    pub read_only: bool,
}

impl TensorlakeWorkspaceRoot {
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
pub struct TensorlakeNativeGrant {
    pub id: GrantId,
    /// Absolute sandbox path beneath which the grant is valid.
    pub path: String,
    pub read_only: bool,
}

#[derive(Clone, Debug)]
pub struct TensorlakeEnvironmentConfig {
    pub machine_id: MachineId,
    pub environment_id: EnvironmentId,
    pub name: String,
    /// Absolute target-side directory used for prepared mutations, artifacts,
    /// idempotency records, and recoverable process journals.
    pub state_directory: String,
    pub workspace_roots: Vec<TensorlakeWorkspaceRoot>,
    pub native_grants: Vec<TensorlakeNativeGrant>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn connection_debug_output_redacts_the_api_key() {
        let config = TensorlakeConnectionConfig::new(
            Url::parse("https://sandbox.sandbox.tensorlake.ai/").unwrap(),
            "tl-secret-value",
        );
        let debug = format!("{config:?}");
        assert!(debug.contains("[REDACTED]"));
        assert!(!debug.contains("tl-secret-value"));
    }
}
