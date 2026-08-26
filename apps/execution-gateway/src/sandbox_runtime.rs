use std::{sync::Arc, time::Duration};

use connector_blaxel::{
    BlaxelConnectionConfig, BlaxelExecutionRuntime, BlaxelRuntimeConfig, BlaxelWorkspaceRoot,
};
use connector_daytona::{
    DaytonaConnectionConfig, DaytonaExecutionRuntime, DaytonaRuntimeConfig, DaytonaWorkspaceRoot,
};
use connector_e2b::{E2bConnectionConfig, E2bExecutionRuntime, E2bRuntimeConfig, E2bWorkspaceRoot};
use connector_tensorlake::{
    TensorlakeConnectionConfig, TensorlakeExecutionRuntime, TensorlakeRuntimeConfig,
    TensorlakeWorkspaceRoot,
};
use execution_contracts::{MachineId, WorkspaceRootId};
use execution_runtime::ExecutionRuntime;
use serde_json::Value;
use thiserror::Error;
use url::Url;

use crate::{
    db::{Database, DbError},
    sandbox_accounts::{SandboxAccountError, SandboxAccountService, SandboxProvider},
    sandbox_machines::{SandboxMachine, SandboxSecretError, SandboxSecretStore},
};

pub const SANDBOX_ROOT_ID: &str = "root";

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct SandboxFilesystemLayout {
    workspace_root: &'static str,
    state_directory: &'static str,
}

const fn filesystem_layout(provider: SandboxProvider) -> SandboxFilesystemLayout {
    match provider {
        SandboxProvider::E2b => SandboxFilesystemLayout {
            workspace_root: "/home/user",
            state_directory: "/home/user/.agent-pane",
        },
        SandboxProvider::Daytona => SandboxFilesystemLayout {
            workspace_root: "/home/daytona",
            state_directory: "/home/daytona/.agent-pane",
        },
        SandboxProvider::Blaxel => SandboxFilesystemLayout {
            workspace_root: "/blaxel",
            state_directory: "/blaxel/.agent-pane",
        },
        SandboxProvider::Tensorlake => SandboxFilesystemLayout {
            workspace_root: "/home/tl-user",
            state_directory: "/home/tl-user/.agent-pane",
        },
    }
}

pub(crate) const fn sandbox_workspace_root(provider: SandboxProvider) -> &'static str {
    filesystem_layout(provider).workspace_root
}

#[derive(Clone)]
pub struct SandboxRuntimeFactory {
    database: Database,
    accounts: SandboxAccountService,
    secrets: SandboxSecretStore,
}

impl SandboxRuntimeFactory {
    pub const fn new(
        database: Database,
        accounts: SandboxAccountService,
        secrets: SandboxSecretStore,
    ) -> Self {
        Self {
            database,
            accounts,
            secrets,
        }
    }

    pub async fn connect(
        &self,
        machine_id: &MachineId,
        machine_name: &str,
    ) -> Result<Arc<dyn ExecutionRuntime>, SandboxRuntimeError> {
        let mut machine = self
            .database
            .sandbox_machine(machine_id.as_str())
            .await?
            .ok_or(SandboxRuntimeError::MachineNotFound)?;
        let credentials = self
            .accounts
            .credentials(machine.sandbox_account_id)
            .await?
            .ok_or(SandboxRuntimeError::AccountCredentialsMissing)?;
        if credentials.provider() != machine.provider {
            return Err(SandboxRuntimeError::ProviderMismatch);
        }
        if machine.provider == SandboxProvider::Daytona {
            let ready = connector_daytona::ensure_started(
                credentials.api_key(),
                &machine.provider_resource_id,
                sandbox_start_timeout(&machine.account_config),
                sandbox_poll_interval(&machine.account_config),
            )
            .await?;
            let toolbox_url = match ready.toolbox_url {
                Some(url) => url,
                None => {
                    connector_daytona::DaytonaConnectionConfig::for_sandbox(
                        &machine.provider_resource_id,
                        credentials.api_key(),
                    )
                    .map_err(|error| SandboxRuntimeError::Configuration(error.to_string()))?
                    .toolbox_url
                }
            };
            let config = machine.connection_config.as_object_mut().ok_or_else(|| {
                SandboxRuntimeError::Configuration(
                    "Daytona connection config must be an object".to_owned(),
                )
            })?;
            config.insert(
                "toolbox_url".to_owned(),
                Value::String(toolbox_url.to_string()),
            );
            config.insert(
                "network_block_all".to_owned(),
                Value::Bool(ready.network_block_all),
            );
        } else if machine.provider == SandboxProvider::Tensorlake {
            let ready = connector_tensorlake::ensure_started(
                credentials.api_key(),
                &machine.provider_resource_id,
                sandbox_start_timeout(&machine.account_config),
                sandbox_poll_interval(&machine.account_config),
            )
            .await?;
            let config = machine.connection_config.as_object_mut().ok_or_else(|| {
                SandboxRuntimeError::Configuration(
                    "Tensorlake connection config must be an object".to_owned(),
                )
            })?;
            config.insert(
                "proxy_url".to_owned(),
                Value::String(ready.sandbox_url.to_string()),
            );
        }
        let connection_secret = self
            .secrets
            .decrypt(machine_id, machine.connection_secret_id)
            .await?;
        build_runtime(
            &machine,
            machine_name,
            credentials.api_key(),
            connection_secret.as_deref().map(String::as_str),
        )
    }
}

pub fn build_runtime(
    machine: &SandboxMachine,
    machine_name: &str,
    api_key: &str,
    connection_secret: Option<&str>,
) -> Result<Arc<dyn ExecutionRuntime>, SandboxRuntimeError> {
    let root_id = WorkspaceRootId::new(SANDBOX_ROOT_ID)
        .map_err(|error| SandboxRuntimeError::Configuration(error.to_string()))?;
    let layout = filesystem_layout(machine.provider);
    match machine.provider {
        SandboxProvider::E2b => {
            let token = connection_secret.ok_or(SandboxRuntimeError::ConnectionSecretMissing)?;
            let mut connection = E2bConnectionConfig::production(
                machine.provider_resource_id.clone(),
                token.to_owned(),
            );
            if let Some(url) = optional_url(&machine.connection_config, "sandbox_url")? {
                connection.sandbox_url = url;
            }
            apply_common_connection(&machine.connection_config, &mut connection.python_command);
            let runtime = E2bExecutionRuntime::connect(
                connection,
                E2bRuntimeConfig {
                    machine_id: machine.machine_id.clone(),
                    name: machine_name.to_owned(),
                    state_directory: layout.state_directory.to_owned(),
                    workspace_roots: vec![E2bWorkspaceRoot {
                        id: root_id,
                        name: "Sandbox workspace".to_owned(),
                        path: layout.workspace_root.to_owned(),
                        read_only: false,
                    }],
                    native_grants: Vec::new(),
                },
            )?;
            Ok(Arc::new(runtime))
        }
        SandboxProvider::Daytona => {
            let toolbox_url = required_url(&machine.connection_config, "toolbox_url")?;
            let mut connection = DaytonaConnectionConfig::new(toolbox_url, api_key.to_owned());
            connection.network_block_all = machine
                .connection_config
                .get("network_block_all")
                .and_then(Value::as_bool)
                .unwrap_or(false);
            apply_common_connection(&machine.connection_config, &mut connection.python_command);
            let runtime = DaytonaExecutionRuntime::connect(
                connection,
                DaytonaRuntimeConfig {
                    machine_id: machine.machine_id.clone(),
                    name: machine_name.to_owned(),
                    state_directory: layout.state_directory.to_owned(),
                    workspace_roots: vec![DaytonaWorkspaceRoot {
                        id: root_id,
                        name: "Sandbox workspace".to_owned(),
                        path: layout.workspace_root.to_owned(),
                        read_only: false,
                    }],
                    native_grants: Vec::new(),
                },
            )?;
            Ok(Arc::new(runtime))
        }
        SandboxProvider::Blaxel => {
            let sandbox_url = required_url(&machine.connection_config, "sandbox_url")?;
            let mut connection = BlaxelConnectionConfig::new(sandbox_url, api_key.to_owned());
            if let Some(workspace) = string(&machine.connection_config, "workspace") {
                connection = connection.with_workspace(workspace);
            }
            apply_common_connection(&machine.connection_config, &mut connection.python_command);
            let runtime = BlaxelExecutionRuntime::connect(
                connection,
                BlaxelRuntimeConfig {
                    machine_id: machine.machine_id.clone(),
                    name: machine_name.to_owned(),
                    state_directory: layout.state_directory.to_owned(),
                    workspace_roots: vec![BlaxelWorkspaceRoot {
                        id: root_id,
                        name: "Sandbox workspace".to_owned(),
                        path: layout.workspace_root.to_owned(),
                        read_only: false,
                    }],
                    native_grants: Vec::new(),
                },
            )?;
            Ok(Arc::new(runtime))
        }
        SandboxProvider::Tensorlake => {
            let proxy_url = required_url(&machine.connection_config, "proxy_url")?;
            let mut connection = TensorlakeConnectionConfig::new(proxy_url, api_key.to_owned());
            if let Some(user) = string(&machine.connection_config, "user") {
                connection.user = user.to_owned();
            }
            apply_common_connection(&machine.connection_config, &mut connection.python_command);
            let runtime = TensorlakeExecutionRuntime::connect(
                connection,
                TensorlakeRuntimeConfig {
                    machine_id: machine.machine_id.clone(),
                    name: machine_name.to_owned(),
                    state_directory: layout.state_directory.to_owned(),
                    workspace_roots: vec![TensorlakeWorkspaceRoot {
                        id: root_id,
                        name: "Sandbox workspace".to_owned(),
                        path: layout.workspace_root.to_owned(),
                        read_only: false,
                    }],
                    native_grants: Vec::new(),
                },
            )?;
            Ok(Arc::new(runtime))
        }
    }
}

pub fn sandbox_machine_record(
    machine_id: MachineId,
    sandbox_account_id: uuid::Uuid,
    provider: SandboxProvider,
    provider_resource_id: String,
    connection_config: Value,
    provider_metadata: Value,
    account_config: Value,
) -> SandboxMachine {
    SandboxMachine {
        machine_id,
        sandbox_account_id,
        provider,
        provider_resource_id,
        connection_secret_id: None,
        connection_config,
        provider_metadata,
        account_config,
    }
}

fn required_url(config: &Value, field: &'static str) -> Result<Url, SandboxRuntimeError> {
    optional_url(config, field)?.ok_or(SandboxRuntimeError::MissingConfiguration(field))
}

fn optional_url(config: &Value, field: &'static str) -> Result<Option<Url>, SandboxRuntimeError> {
    string(config, field)
        .map(|value| {
            Url::parse(value).map_err(|source| {
                SandboxRuntimeError::Configuration(format!("invalid {field}: {source}"))
            })
        })
        .transpose()
}

fn string<'a>(config: &'a Value, field: &'static str) -> Option<&'a str> {
    config.get(field).and_then(Value::as_str)
}

fn apply_common_connection(config: &Value, python_command: &mut String) {
    if let Some(value) = string(config, "python_command") {
        *python_command = value.to_owned();
    }
}

fn sandbox_start_timeout(config: &Value) -> Duration {
    Duration::from_secs(
        config
            .get("provision_timeout_seconds")
            .and_then(Value::as_u64)
            .unwrap_or(120),
    )
}

fn sandbox_poll_interval(config: &Value) -> Duration {
    Duration::from_millis(
        config
            .get("poll_interval_ms")
            .and_then(Value::as_u64)
            .unwrap_or(500),
    )
}

#[derive(Debug, Error)]
pub enum SandboxRuntimeError {
    #[error("sandbox machine was not found or is not running")]
    MachineNotFound,
    #[error("sandbox account credentials are missing")]
    AccountCredentialsMissing,
    #[error("sandbox machine and account providers do not match")]
    ProviderMismatch,
    #[error("sandbox connection secret is missing")]
    ConnectionSecretMissing,
    #[error("sandbox connection configuration is missing {0}")]
    MissingConfiguration(&'static str),
    #[error("sandbox runtime configuration is invalid: {0}")]
    Configuration(String),
    #[error(transparent)]
    Database(#[from] DbError),
    #[error(transparent)]
    Account(#[from] SandboxAccountError),
    #[error(transparent)]
    Secret(#[from] SandboxSecretError),
    #[error(transparent)]
    E2b(#[from] connector_e2b::E2bConnectorError),
    #[error(transparent)]
    Daytona(#[from] connector_daytona::DaytonaConnectorError),
    #[error(transparent)]
    DaytonaLifecycle(#[from] connector_daytona::DaytonaTransportError),
    #[error(transparent)]
    Blaxel(#[from] connector_blaxel::BlaxelConnectorError),
    #[error(transparent)]
    Tensorlake(#[from] connector_tensorlake::TensorlakeConnectorError),
    #[error(transparent)]
    TensorlakeLifecycle(#[from] connector_tensorlake::TensorlakeTransportError),
}

#[cfg(test)]
mod tests {
    use super::{SandboxFilesystemLayout, SandboxProvider, filesystem_layout};

    #[test]
    fn uses_each_providers_writable_workspace() {
        assert_eq!(
            filesystem_layout(SandboxProvider::E2b),
            SandboxFilesystemLayout {
                workspace_root: "/home/user",
                state_directory: "/home/user/.agent-pane",
            }
        );
        assert_eq!(
            filesystem_layout(SandboxProvider::Daytona),
            SandboxFilesystemLayout {
                workspace_root: "/home/daytona",
                state_directory: "/home/daytona/.agent-pane",
            }
        );
        assert_eq!(
            filesystem_layout(SandboxProvider::Blaxel),
            SandboxFilesystemLayout {
                workspace_root: "/blaxel",
                state_directory: "/blaxel/.agent-pane",
            }
        );
        assert_eq!(
            filesystem_layout(SandboxProvider::Tensorlake),
            SandboxFilesystemLayout {
                workspace_root: "/home/tl-user",
                state_directory: "/home/tl-user/.agent-pane",
            }
        );
    }
}
