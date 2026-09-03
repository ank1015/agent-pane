use std::path::{Path, PathBuf};

use execution_core::{ExecutionHostId, RootId};
use execution_supervisor_core::{SupervisorConfig, SupervisorLimits, SupervisorRoot};
use serde::Deserialize;
use thiserror::Error;

#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct HostConfig {
    pub gateway: String,
    #[serde(default)]
    pub allow_insecure_http: bool,
    pub state_directory: PathBuf,
    pub roots: Vec<RootConfig>,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RootConfig {
    pub id: String,
    pub name: String,
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

impl HostConfig {
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
        for root in &mut config.roots {
            absolutize(&mut root.path, base);
        }
        config.validate()?;
        Ok(config)
    }

    pub fn supervisor_config(&self, host_id: uuid::Uuid) -> Result<SupervisorConfig, ConfigError> {
        let host_id = ExecutionHostId::new(host_id.to_string())
            .map_err(|error| ConfigError::Invalid(error.to_string()))?;
        let roots = self
            .roots
            .iter()
            .map(|root| {
                Ok(SupervisorRoot {
                    id: RootId::new(root.id.clone())
                        .map_err(|error| ConfigError::Invalid(error.to_string()))?,
                    name: root.name.clone(),
                    path: root.path.clone(),
                    read_only: root.read_only,
                })
            })
            .collect::<Result<Vec<_>, ConfigError>>()?;
        Ok(SupervisorConfig {
            host_id,
            state_directory: self.state_directory.join("supervisor"),
            roots,
            limits: SupervisorLimits::default(),
        })
    }

    fn validate(&self) -> Result<(), ConfigError> {
        let url = url::Url::parse(&self.gateway).map_err(|error| {
            ConfigError::Invalid(format!("gateway is not a valid URL: {error}"))
        })?;
        if !matches!(url.scheme(), "http" | "https") {
            return Err(ConfigError::Invalid(
                "gateway must use http or https".to_owned(),
            ));
        }
        if url.scheme() != "https" && !self.allow_insecure_http {
            return Err(ConfigError::Invalid(
                "gateway must use https unless allow_insecure_http is true".to_owned(),
            ));
        }
        if self.state_directory.as_os_str().is_empty() {
            return Err(ConfigError::Invalid(
                "state_directory must not be empty".to_owned(),
            ));
        }
        if self.roots.is_empty() {
            return Err(ConfigError::Invalid(
                "at least one filesystem root is required".to_owned(),
            ));
        }
        for root in &self.roots {
            RootId::new(root.id.clone())
                .map_err(|error| ConfigError::Invalid(error.to_string()))?;
            if root.name.trim().is_empty() || root.name.trim() != root.name {
                return Err(ConfigError::Invalid(
                    "root names must not be blank or have surrounding whitespace".to_owned(),
                ));
            }
        }
        Ok(())
    }
}

fn absolutize(path: &mut PathBuf, base: &Path) {
    if path.is_relative() {
        *path = base.join(&*path);
    }
}
