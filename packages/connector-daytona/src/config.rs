use execution_contracts::{GrantId, MachineId, WorkspaceRoot, WorkspaceRootId};
use url::Url;

#[derive(Clone)]
pub struct DaytonaConnectionConfig {
    /// Toolbox base URL for one sandbox, normally
    /// `https://proxy.app.daytona.io/toolbox/<sandbox-id>/`.
    pub toolbox_url: Url,
    pub api_key: String,
    pub request_timeout_ms: u64,
    pub python_command: String,
    /// Set this from the sandbox metadata when its outbound network is
    /// already fully blocked. Daytona firewall settings are sandbox-wide.
    pub network_block_all: bool,
}

impl std::fmt::Debug for DaytonaConnectionConfig {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("DaytonaConnectionConfig")
            .field("toolbox_url", &self.toolbox_url)
            .field("api_key", &"[REDACTED]")
            .field("request_timeout_ms", &self.request_timeout_ms)
            .field("python_command", &self.python_command)
            .field("network_block_all", &self.network_block_all)
            .finish()
    }
}

impl DaytonaConnectionConfig {
    pub fn new(mut toolbox_url: Url, api_key: impl Into<String>) -> Self {
        if !toolbox_url.path().ends_with('/') {
            toolbox_url.set_path(&format!("{}/", toolbox_url.path()));
        }
        Self {
            toolbox_url,
            api_key: api_key.into(),
            request_timeout_ms: 60_000,
            python_command: "python3".to_owned(),
            network_block_all: false,
        }
    }

    pub fn for_sandbox(
        sandbox_id: &str,
        api_key: impl Into<String>,
    ) -> Result<Self, url::ParseError> {
        Url::parse(&format!(
            "https://proxy.app.daytona.io/toolbox/{sandbox_id}/"
        ))
        .map(|url| Self::new(url, api_key))
    }
}

#[derive(Clone, Debug)]
pub struct DaytonaWorkspaceRoot {
    pub id: WorkspaceRootId,
    pub name: String,
    pub path: String,
    pub read_only: bool,
}

impl DaytonaWorkspaceRoot {
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
pub struct DaytonaNativeGrant {
    pub id: GrantId,
    /// Absolute sandbox path beneath which the grant is valid.
    pub path: String,
    pub read_only: bool,
}

#[derive(Clone, Debug)]
pub struct DaytonaRuntimeConfig {
    pub machine_id: MachineId,
    pub name: String,
    /// Absolute target-side directory used for prepared mutations, artifacts,
    /// idempotency records, and recoverable process journals.
    pub state_directory: String,
    pub workspace_roots: Vec<DaytonaWorkspaceRoot>,
    pub native_grants: Vec<DaytonaNativeGrant>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn connection_debug_output_redacts_the_api_key() {
        let config = DaytonaConnectionConfig::new(
            Url::parse("https://proxy.app.daytona.io/toolbox/sandbox-id/").unwrap(),
            "daytona-secret-value",
        );
        let debug = format!("{config:?}");
        assert!(debug.contains("[REDACTED]"));
        assert!(!debug.contains("daytona-secret-value"));
    }

    #[test]
    fn connection_normalizes_the_toolbox_base_path() {
        let config = DaytonaConnectionConfig::new(
            Url::parse("https://proxy.app.daytona.io/toolbox/sandbox-id").unwrap(),
            "secret",
        );
        assert_eq!(config.toolbox_url.path(), "/toolbox/sandbox-id/");
    }
}
