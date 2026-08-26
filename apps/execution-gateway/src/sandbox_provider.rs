use std::time::Duration;

use async_trait::async_trait;
use serde_json::{Value, json};
use thiserror::Error;

use crate::{
    sandbox_accounts::{SandboxAccount, SandboxCredentials, SandboxProvider},
    snapshots::Snapshot,
};

#[derive(Clone, Debug)]
pub struct ProvisionedSandbox {
    pub provider_resource_id: String,
    pub connection_config: Value,
    pub connection_secret: Option<String>,
    pub provider_metadata: Value,
}

#[async_trait]
pub trait SandboxProviderClient: Send + Sync {
    async fn create_sandbox(
        &self,
        account: &SandboxAccount,
        credentials: &SandboxCredentials,
        target_name: &str,
        source: Option<&str>,
    ) -> Result<ProvisionedSandbox, SandboxProviderError>;

    async fn create_snapshot(
        &self,
        account: &SandboxAccount,
        sandbox_id: &str,
        snapshot_name: &str,
        credentials: &SandboxCredentials,
    ) -> Result<String, SandboxProviderError>;

    async fn create_from_snapshot(
        &self,
        snapshot: &Snapshot,
        account: &SandboxAccount,
        credentials: &SandboxCredentials,
        target_name: &str,
    ) -> Result<ProvisionedSandbox, SandboxProviderError>;

    async fn terminate(
        &self,
        provider: SandboxProvider,
        account_config: &Value,
        credentials: &SandboxCredentials,
        provider_resource_id: &str,
    ) -> Result<(), SandboxProviderError>;
}

#[derive(Default)]
pub struct HttpSandboxProviderClient;

#[async_trait]
impl SandboxProviderClient for HttpSandboxProviderClient {
    async fn create_sandbox(
        &self,
        account: &SandboxAccount,
        credentials: &SandboxCredentials,
        target_name: &str,
        source: Option<&str>,
    ) -> Result<ProvisionedSandbox, SandboxProviderError> {
        match account.provider {
            SandboxProvider::E2b => {
                let template_id = source.ok_or(SandboxProviderError::MissingCreationSource)?;
                let sandbox =
                    connector_e2b::create_details(credentials.api_key(), template_id).await?;
                Ok(ProvisionedSandbox {
                    provider_resource_id: sandbox.sandbox_id,
                    connection_config: json!({"sandbox_url": "https://sandbox.e2b.app/"}),
                    connection_secret: Some(sandbox.envd_access_token),
                    provider_metadata: json!({
                        "sandbox_domain": sandbox.sandbox_domain,
                        "created_from": template_id,
                        "template_id": template_id,
                    }),
                })
            }
            SandboxProvider::Daytona => {
                let sandbox_id =
                    connector_daytona::create(credentials.api_key(), Some(target_name)).await?;
                ready_daytona_sandbox(
                    credentials,
                    account,
                    sandbox_id,
                    json!({"created_from": "default"}),
                )
                .await
            }
            SandboxProvider::Blaxel => {
                let workspace = blaxel_workspace(&account.config, credentials).await?;
                let sandbox_id =
                    connector_blaxel::create(credentials.api_key(), &workspace, target_name)
                        .await?;
                ready_blaxel_sandbox(
                    credentials,
                    account,
                    workspace,
                    sandbox_id,
                    json!({"created_from": "default"}),
                )
                .await
            }
            SandboxProvider::Tensorlake => {
                let sandbox_id =
                    connector_tensorlake::create(credentials.api_key(), target_name).await?;
                ready_tensorlake_sandbox(
                    credentials,
                    account,
                    sandbox_id,
                    json!({"created_from": "default"}),
                )
                .await
            }
        }
    }

    async fn create_snapshot(
        &self,
        account: &SandboxAccount,
        sandbox_id: &str,
        snapshot_name: &str,
        credentials: &SandboxCredentials,
    ) -> Result<String, SandboxProviderError> {
        match account.provider {
            SandboxProvider::E2b => {
                connector_e2b::create_snapshot(credentials.api_key(), sandbox_id)
                    .await
                    .map_err(Into::into)
            }
            SandboxProvider::Daytona => {
                connector_daytona::create_snapshot(credentials.api_key(), sandbox_id, snapshot_name)
                    .await
                    .map_err(Into::into)
            }
            SandboxProvider::Blaxel => {
                let workspace = blaxel_workspace(&account.config, credentials).await?;
                connector_blaxel::create_snapshot(
                    credentials.api_key(),
                    &workspace,
                    sandbox_id,
                    snapshot_name,
                )
                .await
                .map_err(Into::into)
            }
            SandboxProvider::Tensorlake => {
                connector_tensorlake::create_snapshot(credentials.api_key(), sandbox_id)
                    .await
                    .map_err(Into::into)
            }
        }
    }

    async fn create_from_snapshot(
        &self,
        snapshot: &Snapshot,
        account: &SandboxAccount,
        credentials: &SandboxCredentials,
        target_name: &str,
    ) -> Result<ProvisionedSandbox, SandboxProviderError> {
        match snapshot.provider {
            SandboxProvider::E2b => {
                let sandbox = connector_e2b::create_from_snapshot_details(
                    credentials.api_key(),
                    &snapshot.provider_snapshot_id,
                )
                .await?;
                Ok(ProvisionedSandbox {
                    provider_resource_id: sandbox.sandbox_id,
                    connection_config: json!({"sandbox_url": "https://sandbox.e2b.app/"}),
                    connection_secret: Some(sandbox.envd_access_token),
                    provider_metadata: json!({
                        "sandbox_domain": sandbox.sandbox_domain,
                        "template_id": snapshot.provider_snapshot_id,
                    }),
                })
            }
            SandboxProvider::Daytona => {
                let sandbox_id = connector_daytona::create_from_snapshot(
                    credentials.api_key(),
                    &snapshot.provider_snapshot_id,
                )
                .await?;
                ready_daytona_sandbox(
                    credentials,
                    account,
                    sandbox_id,
                    json!({"created_from": snapshot.provider_snapshot_id}),
                )
                .await
            }
            SandboxProvider::Blaxel => {
                let workspace = blaxel_workspace(&account.config, credentials).await?;
                let sandbox_id = connector_blaxel::create_from_snapshot(
                    credentials.api_key(),
                    &workspace,
                    &snapshot.sandbox_id,
                    &snapshot.provider_snapshot_id,
                    target_name,
                )
                .await?;
                ready_blaxel_sandbox(
                    credentials,
                    account,
                    workspace,
                    sandbox_id,
                    json!({"created_from": snapshot.provider_snapshot_id}),
                )
                .await
            }
            SandboxProvider::Tensorlake => {
                let sandbox_id = connector_tensorlake::create_from_snapshot(
                    credentials.api_key(),
                    &snapshot.provider_snapshot_id,
                    target_name,
                )
                .await?;
                ready_tensorlake_sandbox(
                    credentials,
                    account,
                    sandbox_id,
                    json!({"created_from": snapshot.provider_snapshot_id}),
                )
                .await
            }
        }
    }

    async fn terminate(
        &self,
        provider: SandboxProvider,
        account_config: &Value,
        credentials: &SandboxCredentials,
        provider_resource_id: &str,
    ) -> Result<(), SandboxProviderError> {
        match provider {
            SandboxProvider::E2b => {
                connector_e2b::terminate(credentials.api_key(), provider_resource_id).await?
            }
            SandboxProvider::Daytona => {
                connector_daytona::terminate(credentials.api_key(), provider_resource_id).await?
            }
            SandboxProvider::Blaxel => {
                let workspace = blaxel_workspace(account_config, credentials).await?;
                connector_blaxel::terminate(credentials.api_key(), &workspace, provider_resource_id)
                    .await?
            }
            SandboxProvider::Tensorlake => {
                connector_tensorlake::terminate(credentials.api_key(), provider_resource_id).await?
            }
        }
        Ok(())
    }
}

async fn ready_blaxel_sandbox(
    credentials: &SandboxCredentials,
    account: &SandboxAccount,
    workspace: String,
    sandbox_id: String,
    provider_metadata: Value,
) -> Result<ProvisionedSandbox, SandboxProviderError> {
    let ready = match connector_blaxel::wait_until_ready(
        credentials.api_key(),
        &workspace,
        &sandbox_id,
        provision_timeout(&account.config),
        poll_interval(&account.config),
    )
    .await
    {
        Ok(ready) => ready,
        Err(error) => {
            if let Err(cleanup) =
                connector_blaxel::terminate(credentials.api_key(), &workspace, &sandbox_id).await
            {
                tracing::error!(%cleanup, %sandbox_id, "failed to clean up Blaxel provisioning failure");
            }
            return Err(error.into());
        }
    };
    Ok(ProvisionedSandbox {
        provider_resource_id: sandbox_id,
        connection_config: json!({
            "sandbox_url": ready.sandbox_url.as_str(),
            "workspace": workspace,
        }),
        connection_secret: None,
        provider_metadata,
    })
}

async fn ready_daytona_sandbox(
    credentials: &SandboxCredentials,
    account: &SandboxAccount,
    sandbox_id: String,
    provider_metadata: Value,
) -> Result<ProvisionedSandbox, SandboxProviderError> {
    let ready = match connector_daytona::wait_until_ready(
        credentials.api_key(),
        &sandbox_id,
        provision_timeout(&account.config),
        poll_interval(&account.config),
    )
    .await
    {
        Ok(ready) => ready,
        Err(error) => {
            if let Err(cleanup) =
                connector_daytona::terminate(credentials.api_key(), &sandbox_id).await
            {
                tracing::error!(%cleanup, %sandbox_id, "failed to clean up Daytona provisioning failure");
            }
            return Err(error.into());
        }
    };
    let toolbox_url = match ready.toolbox_url {
        Some(url) => url,
        None => {
            connector_daytona::DaytonaConnectionConfig::for_sandbox(
                &sandbox_id,
                credentials.api_key(),
            )
            .map_err(|error| SandboxProviderError::Configuration(error.to_string()))?
            .toolbox_url
        }
    };
    Ok(ProvisionedSandbox {
        provider_resource_id: sandbox_id,
        connection_config: json!({
            "toolbox_url": toolbox_url.as_str(),
            "network_block_all": ready.network_block_all,
        }),
        connection_secret: None,
        provider_metadata,
    })
}

async fn ready_tensorlake_sandbox(
    credentials: &SandboxCredentials,
    account: &SandboxAccount,
    sandbox_id: String,
    provider_metadata: Value,
) -> Result<ProvisionedSandbox, SandboxProviderError> {
    let ready = match connector_tensorlake::wait_until_ready(
        credentials.api_key(),
        &sandbox_id,
        provision_timeout(&account.config),
        poll_interval(&account.config),
    )
    .await
    {
        Ok(ready) => ready,
        Err(error) => {
            if let Err(cleanup) =
                connector_tensorlake::terminate(credentials.api_key(), &sandbox_id).await
            {
                tracing::error!(%cleanup, %sandbox_id, "failed to clean up Tensorlake provisioning failure");
            }
            return Err(error.into());
        }
    };
    Ok(ProvisionedSandbox {
        provider_resource_id: sandbox_id,
        connection_config: json!({"proxy_url": ready.sandbox_url.as_str()}),
        connection_secret: None,
        provider_metadata,
    })
}

fn provision_timeout(config: &Value) -> Duration {
    Duration::from_secs(
        config
            .get("provision_timeout_seconds")
            .and_then(Value::as_u64)
            .unwrap_or(120),
    )
}

fn poll_interval(config: &Value) -> Duration {
    Duration::from_millis(
        config
            .get("poll_interval_ms")
            .and_then(Value::as_u64)
            .unwrap_or(500),
    )
}

async fn blaxel_workspace(
    config: &Value,
    credentials: &SandboxCredentials,
) -> Result<String, SandboxProviderError> {
    if let Some(workspace) = config
        .get("workspace")
        .and_then(Value::as_str)
        .filter(|value| !value.trim().is_empty())
    {
        return Ok(workspace.to_owned());
    }
    connector_blaxel::resolve_workspace(credentials.api_key())
        .await
        .map_err(Into::into)
}

#[derive(Debug, Error)]
pub enum SandboxProviderError {
    #[error("sandbox provider configuration is invalid: {0}")]
    Configuration(String),
    #[error("sandbox creation requires a provider source")]
    MissingCreationSource,
    #[error("{0} does not support this sandbox operation")]
    UnsupportedOperation(SandboxProvider),
    #[error(transparent)]
    E2b(#[from] connector_e2b::E2bTransportError),
    #[error(transparent)]
    Daytona(#[from] connector_daytona::DaytonaTransportError),
    #[error(transparent)]
    Blaxel(#[from] connector_blaxel::BlaxelTransportError),
    #[error(transparent)]
    Tensorlake(#[from] connector_tensorlake::TensorlakeTransportError),
}
