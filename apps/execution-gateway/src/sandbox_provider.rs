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
    async fn create_e2b(
        &self,
        template_id: &str,
        credentials: &SandboxCredentials,
    ) -> Result<ProvisionedSandbox, SandboxProviderError>;

    async fn create_e2b_snapshot(
        &self,
        sandbox_id: &str,
        credentials: &SandboxCredentials,
    ) -> Result<String, SandboxProviderError>;

    async fn create(
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
    async fn create_e2b(
        &self,
        template_id: &str,
        credentials: &SandboxCredentials,
    ) -> Result<ProvisionedSandbox, SandboxProviderError> {
        let sandbox = connector_e2b::create_details(credentials.api_key(), template_id).await?;
        Ok(ProvisionedSandbox {
            provider_resource_id: sandbox.sandbox_id,
            connection_config: json!({"sandbox_url": "https://sandbox.e2b.app/"}),
            connection_secret: Some(sandbox.envd_access_token),
            provider_metadata: json!({
                "sandbox_domain": sandbox.sandbox_domain,
                "template_id": template_id,
            }),
        })
    }

    async fn create_e2b_snapshot(
        &self,
        sandbox_id: &str,
        credentials: &SandboxCredentials,
    ) -> Result<String, SandboxProviderError> {
        connector_e2b::create_snapshot(credentials.api_key(), sandbox_id)
            .await
            .map_err(Into::into)
    }

    async fn create(
        &self,
        snapshot: &Snapshot,
        account: &SandboxAccount,
        credentials: &SandboxCredentials,
        target_name: &str,
    ) -> Result<ProvisionedSandbox, SandboxProviderError> {
        let timeout = provision_timeout(&account.config);
        let poll_interval = poll_interval(&account.config);
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
                let ready = match connector_daytona::wait_until_ready(
                    credentials.api_key(),
                    &sandbox_id,
                    timeout,
                    poll_interval,
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
                    provider_metadata: json!({}),
                })
            }
            SandboxProvider::Blaxel => {
                let workspace = required_config_string(&account.config, "workspace")?;
                let sandbox_id = connector_blaxel::create_from_snapshot(
                    credentials.api_key(),
                    workspace,
                    &snapshot.sandbox_id,
                    &snapshot.provider_snapshot_id,
                    target_name,
                )
                .await?;
                let ready = match connector_blaxel::wait_until_ready(
                    credentials.api_key(),
                    workspace,
                    &sandbox_id,
                    timeout,
                    poll_interval,
                )
                .await
                {
                    Ok(ready) => ready,
                    Err(error) => {
                        if let Err(cleanup) = connector_blaxel::terminate(
                            credentials.api_key(),
                            workspace,
                            &sandbox_id,
                        )
                        .await
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
                    provider_metadata: json!({}),
                })
            }
            SandboxProvider::Tensorlake => {
                let sandbox_id = connector_tensorlake::create_from_snapshot(
                    credentials.api_key(),
                    &snapshot.provider_snapshot_id,
                )
                .await?;
                let ready = match connector_tensorlake::wait_until_ready(
                    credentials.api_key(),
                    &sandbox_id,
                    timeout,
                    poll_interval,
                )
                .await
                {
                    Ok(ready) => ready,
                    Err(error) => {
                        if let Err(cleanup) =
                            connector_tensorlake::terminate(credentials.api_key(), &sandbox_id)
                                .await
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
                    provider_metadata: json!({}),
                })
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
                connector_blaxel::terminate(
                    credentials.api_key(),
                    required_config_string(account_config, "workspace")?,
                    provider_resource_id,
                )
                .await?
            }
            SandboxProvider::Tensorlake => {
                connector_tensorlake::terminate(credentials.api_key(), provider_resource_id).await?
            }
        }
        Ok(())
    }
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

fn required_config_string<'a>(
    config: &'a Value,
    field: &'static str,
) -> Result<&'a str, SandboxProviderError> {
    config
        .get(field)
        .and_then(Value::as_str)
        .filter(|value| !value.trim().is_empty())
        .ok_or(SandboxProviderError::MissingConfiguration(field))
}

#[derive(Debug, Error)]
pub enum SandboxProviderError {
    #[error("sandbox account config is missing {0}")]
    MissingConfiguration(&'static str),
    #[error("sandbox provider configuration is invalid: {0}")]
    Configuration(String),
    #[error(transparent)]
    E2b(#[from] connector_e2b::E2bTransportError),
    #[error(transparent)]
    Daytona(#[from] connector_daytona::DaytonaTransportError),
    #[error(transparent)]
    Blaxel(#[from] connector_blaxel::BlaxelTransportError),
    #[error(transparent)]
    Tensorlake(#[from] connector_tensorlake::TensorlakeTransportError),
}
