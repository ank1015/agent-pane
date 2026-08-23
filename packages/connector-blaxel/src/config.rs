use execution_contracts::{EnvironmentId, GrantId, MachineId, WorkspaceRoot, WorkspaceRootId};
use url::Url;

#[derive(Clone)]
pub struct BlaxelConnectionConfig {
    /// Sandbox API endpoint returned in the sandbox resource metadata, such as
    /// `https://sbx-<sandbox>-<workspace>.<region>.bl.run/`.
    pub sandbox_url: Url,
    pub api_key: String,
    /// Optional workspace header. Service-account keys should normally set it.
    pub workspace: Option<String>,
    pub request_timeout_ms: u64,
    pub python_command: String,
}

impl std::fmt::Debug for BlaxelConnectionConfig {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("BlaxelConnectionConfig")
            .field("sandbox_url", &self.sandbox_url)
            .field("api_key", &"[REDACTED]")
            .field("workspace", &self.workspace)
            .field("request_timeout_ms", &self.request_timeout_ms)
            .field("python_command", &self.python_command)
            .finish()
    }
}

impl BlaxelConnectionConfig {
    pub fn new(sandbox_url: Url, api_key: impl Into<String>) -> Self {
        Self {
            sandbox_url,
            api_key: api_key.into(),
            workspace: None,
            request_timeout_ms: 60_000,
            python_command: "python3".to_owned(),
        }
    }

    pub fn with_workspace(mut self, workspace: impl Into<String>) -> Self {
        self.workspace = Some(workspace.into());
        self
    }

    pub fn for_sandbox(
        sandbox: &str,
        workspace: &str,
        region: &str,
        api_key: impl Into<String>,
    ) -> Result<Self, url::ParseError> {
        Url::parse(&format!(
            "https://sbx-{sandbox}-{workspace}.{region}.bl.run/"
        ))
        .map(|url| Self::new(url, api_key).with_workspace(workspace))
    }
}

#[derive(Clone, Debug)]
pub struct BlaxelWorkspaceRoot {
    pub id: WorkspaceRootId,
    pub name: String,
    pub path: String,
    pub read_only: bool,
}

impl BlaxelWorkspaceRoot {
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
pub struct BlaxelNativeGrant {
    pub id: GrantId,
    /// Absolute sandbox path beneath which the grant is valid.
    pub path: String,
    pub read_only: bool,
}

#[derive(Clone, Debug)]
pub struct BlaxelEnvironmentConfig {
    pub machine_id: MachineId,
    pub environment_id: EnvironmentId,
    pub name: String,
    /// Absolute target-side directory used for prepared mutations, artifacts,
    /// idempotency records, and recoverable process journals.
    pub state_directory: String,
    pub workspace_roots: Vec<BlaxelWorkspaceRoot>,
    pub native_grants: Vec<BlaxelNativeGrant>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn connection_debug_output_redacts_the_api_key() {
        let config = BlaxelConnectionConfig::new(
            Url::parse("https://sbx-example-workspace.us-pdx-1.bl.run/").unwrap(),
            "bl-secret-value",
        );
        let debug = format!("{config:?}");
        assert!(debug.contains("[REDACTED]"));
        assert!(!debug.contains("bl-secret-value"));
    }

    #[test]
    fn constructs_a_hosted_sandbox_url() {
        let config = BlaxelConnectionConfig::for_sandbox(
            "sandbox-one",
            "workspace-one",
            "us-pdx-1",
            "secret",
        )
        .unwrap();
        assert_eq!(
            config.sandbox_url.as_str(),
            "https://sbx-sandbox-one-workspace-one.us-pdx-1.bl.run/"
        );
        assert_eq!(config.workspace.as_deref(), Some("workspace-one"));
    }
}
