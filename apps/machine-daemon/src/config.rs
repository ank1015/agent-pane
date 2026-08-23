use std::{
    net::SocketAddr,
    path::{Path, PathBuf},
};

use execution_contracts::{EnvironmentId, GrantId, MachineId, WorkspaceRootId};
use execution_local::{LocalExecutionConfig, LocalNativeGrant, LocalWorkspaceRoot};
use serde::Deserialize;
use thiserror::Error;

#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DaemonConfig {
    #[serde(default)]
    pub machine_id: Option<String>,
    pub environment_id: String,
    pub name: String,
    pub state_directory: PathBuf,
    #[serde(default = "default_listen")]
    pub listen: SocketAddr,
    #[serde(default)]
    pub gateway: Option<String>,
    #[serde(default)]
    pub auth: AuthConfig,
    pub workspace_roots: Vec<WorkspaceRootConfig>,
    #[serde(default)]
    pub native_grants: Vec<NativeGrantConfig>,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AuthConfig {
    #[serde(default)]
    pub token: Option<String>,
    #[serde(default = "default_token_env")]
    pub token_env: String,
}

impl Default for AuthConfig {
    fn default() -> Self {
        Self {
            token: None,
            token_env: default_token_env(),
        }
    }
}

#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct WorkspaceRootConfig {
    pub id: String,
    pub name: String,
    pub path: PathBuf,
    #[serde(default)]
    pub read_only: bool,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct NativeGrantConfig {
    pub id: String,
    pub path: PathBuf,
    #[serde(default)]
    pub read_only: bool,
}

#[derive(Debug, Error)]
pub enum ConfigError {
    #[error("failed to read configuration {}: {source}", path.display())]
    Read {
        path: PathBuf,
        source: std::io::Error,
    },
    #[error("configuration {} is not valid JSON: {source}", path.display())]
    Json {
        path: PathBuf,
        source: serde_json::Error,
    },
    #[error("invalid configuration: {0}")]
    Invalid(String),
}

impl DaemonConfig {
    pub async fn load(path: &Path) -> Result<Self, ConfigError> {
        let bytes = tokio::fs::read(path)
            .await
            .map_err(|source| ConfigError::Read {
                path: path.to_owned(),
                source,
            })?;
        let mut config: Self =
            serde_json::from_slice(&bytes).map_err(|source| ConfigError::Json {
                path: path.to_owned(),
                source,
            })?;
        let base = path.parent().unwrap_or_else(|| Path::new("."));
        absolutize(&mut config.state_directory, base);
        for root in &mut config.workspace_roots {
            absolutize(&mut root.path, base);
        }
        for grant in &mut config.native_grants {
            absolutize(&mut grant.path, base);
        }
        config.validate()?;
        Ok(config)
    }

    pub fn auth_token(&self) -> Result<Option<String>, ConfigError> {
        let token = self.auth.token.clone().or_else(|| {
            std::env::var(&self.auth.token_env)
                .ok()
                .filter(|value| !value.trim().is_empty())
        });
        Ok(token)
    }

    pub fn gateway_url(&self, override_url: Option<String>) -> Result<String, ConfigError> {
        override_url
            .or_else(|| self.gateway.clone())
            .ok_or_else(|| {
                ConfigError::Invalid(
                    "connect requires --gateway or a gateway value in the config".to_owned(),
                )
            })
    }

    pub fn local_config(&self, machine_id: MachineId) -> Result<LocalExecutionConfig, ConfigError> {
        let environment_id = EnvironmentId::new(self.environment_id.clone())
            .map_err(|source| ConfigError::Invalid(source.to_string()))?;
        let roots = self
            .workspace_roots
            .iter()
            .map(|root| {
                Ok(LocalWorkspaceRoot {
                    id: WorkspaceRootId::new(root.id.clone())
                        .map_err(|source| ConfigError::Invalid(source.to_string()))?,
                    name: root.name.clone(),
                    path: root.path.clone(),
                    read_only: root.read_only,
                })
            })
            .collect::<Result<Vec<_>, ConfigError>>()?;
        let grants = self
            .native_grants
            .iter()
            .map(|grant| {
                Ok(LocalNativeGrant {
                    id: GrantId::new(grant.id.clone())
                        .map_err(|source| ConfigError::Invalid(source.to_string()))?,
                    path: grant.path.clone(),
                    read_only: grant.read_only,
                })
            })
            .collect::<Result<Vec<_>, ConfigError>>()?;
        Ok(LocalExecutionConfig {
            machine_id,
            environment_id,
            name: self.name.clone(),
            state_directory: self.state_directory.clone(),
            workspace_roots: roots,
            native_grants: grants,
        })
    }

    fn validate(&self) -> Result<(), ConfigError> {
        if self.name.trim().is_empty() {
            return Err(ConfigError::Invalid("name must not be empty".to_owned()));
        }
        if self.workspace_roots.is_empty() {
            return Err(ConfigError::Invalid(
                "at least one workspace root is required".to_owned(),
            ));
        }
        if let Some(machine_id) = &self.machine_id {
            MachineId::new(machine_id.clone())
                .map_err(|source| ConfigError::Invalid(source.to_string()))?;
        }
        Ok(())
    }
}

fn absolutize(path: &mut PathBuf, base: &Path) {
    if path.is_relative() {
        *path = base.join(&*path);
    }
}

fn default_listen() -> SocketAddr {
    SocketAddr::from(([127, 0, 0, 1], 8787))
}

fn default_token_env() -> String {
    "MACHINE_DAEMON_TOKEN".to_owned()
}
