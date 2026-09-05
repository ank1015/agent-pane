use std::{sync::Arc, time::Duration};

use async_trait::async_trait;
use execution_core::{ExecutionHostDescriptor, ExecutionHostId, OperationContext};
use execution_e2b::{
    E2bApiKey, E2bConfig, E2bControlClient, E2bError, E2bExecutionRuntime, E2bRuntimeConfig,
    SandboxState, SupervisorConfig,
};
use execution_wire::{RequestEnvelope, ResponseEnvelope, dispatch_request};
use url::Url;

#[derive(Clone, Debug)]
pub struct E2bProviderSettings {
    pub control_base_url: Url,
    pub envd_base_url_override: Option<Url>,
    pub request_timeout: Duration,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ProviderHostState {
    Running,
    Paused,
    Other,
}

#[derive(Clone, Debug)]
pub struct ProviderHostDetails {
    pub state: ProviderHostState,
}

#[derive(Clone, Debug)]
pub struct ProviderExecutionResponse {
    pub descriptor: ExecutionHostDescriptor,
    pub response: ResponseEnvelope,
}

#[async_trait]
pub trait E2bProvider: Send + Sync {
    async fn verify_credentials(&self, api_key: &str) -> Result<(), E2bError>;
    async fn create_base(
        &self,
        api_key: &str,
        timeout_seconds: u64,
        network_access: bool,
        ram_mb: u32,
    ) -> Result<String, E2bError>;
    async fn create_from_snapshot(
        &self,
        api_key: &str,
        snapshot_id: &str,
        timeout_seconds: u64,
        network_access: bool,
    ) -> Result<String, E2bError>;
    async fn get_host(
        &self,
        api_key: &str,
        sandbox_id: &str,
        timeout_seconds: u64,
    ) -> Result<ProviderHostDetails, E2bError>;
    async fn pause_host(
        &self,
        api_key: &str,
        sandbox_id: &str,
        timeout_seconds: u64,
    ) -> Result<(), E2bError>;
    async fn resume_host(
        &self,
        api_key: &str,
        sandbox_id: &str,
        timeout_seconds: u64,
    ) -> Result<(), E2bError>;
    async fn delete_host(
        &self,
        api_key: &str,
        sandbox_id: &str,
        timeout_seconds: u64,
    ) -> Result<(), E2bError>;
    async fn create_snapshot(
        &self,
        api_key: &str,
        sandbox_id: &str,
        timeout_seconds: u64,
    ) -> Result<String, E2bError>;
    async fn delete_snapshot(&self, api_key: &str, snapshot_id: &str) -> Result<(), E2bError>;
    async fn execute(
        &self,
        api_key: &str,
        sandbox_id: &str,
        host_id: ExecutionHostId,
        timeout_seconds: u64,
        request: RequestEnvelope,
    ) -> Result<ProviderExecutionResponse, E2bError>;
}

#[derive(Clone, Debug)]
pub struct RealE2bProvider {
    settings: E2bProviderSettings,
}

impl RealE2bProvider {
    pub fn new(settings: E2bProviderSettings) -> Self {
        Self { settings }
    }

    fn config(&self, api_key: &str, timeout_seconds: u64) -> Result<E2bConfig, E2bError> {
        let mut config = E2bConfig::new(E2bApiKey::new(api_key)?)?;
        config.control_base_url = self.settings.control_base_url.clone();
        config.envd_base_url_override = self.settings.envd_base_url_override.clone();
        config.request_timeout = self.settings.request_timeout;
        config.sandbox_timeout_seconds = timeout_seconds;
        Ok(config)
    }
}

#[async_trait]
impl E2bProvider for RealE2bProvider {
    async fn verify_credentials(&self, api_key: &str) -> Result<(), E2bError> {
        E2bControlClient::new(self.config(api_key, 300)?)?
            .verify_credentials()
            .await
    }

    async fn create_base(
        &self,
        api_key: &str,
        timeout_seconds: u64,
        network_access: bool,
        ram_mb: u32,
    ) -> Result<String, E2bError> {
        E2bControlClient::new(self.config(api_key, timeout_seconds)?)?
            .create_base_sandbox(Some(network_access), Some(ram_mb))
            .await
    }

    async fn create_from_snapshot(
        &self,
        api_key: &str,
        snapshot_id: &str,
        timeout_seconds: u64,
        network_access: bool,
    ) -> Result<String, E2bError> {
        E2bControlClient::new(self.config(api_key, timeout_seconds)?)?
            .create_snapshot_sandbox(snapshot_id, Some(network_access))
            .await
    }

    async fn get_host(
        &self,
        api_key: &str,
        sandbox_id: &str,
        timeout_seconds: u64,
    ) -> Result<ProviderHostDetails, E2bError> {
        let details = E2bControlClient::new(self.config(api_key, timeout_seconds)?)?
            .get_sandbox(sandbox_id)
            .await?;
        let state = match details.state {
            SandboxState::Running => ProviderHostState::Running,
            SandboxState::Paused => ProviderHostState::Paused,
            SandboxState::Other(_) => ProviderHostState::Other,
        };
        Ok(ProviderHostDetails { state })
    }

    async fn pause_host(
        &self,
        api_key: &str,
        sandbox_id: &str,
        timeout_seconds: u64,
    ) -> Result<(), E2bError> {
        E2bControlClient::new(self.config(api_key, timeout_seconds)?)?
            .pause_sandbox(sandbox_id)
            .await
    }

    async fn resume_host(
        &self,
        api_key: &str,
        sandbox_id: &str,
        timeout_seconds: u64,
    ) -> Result<(), E2bError> {
        E2bControlClient::new(self.config(api_key, timeout_seconds)?)?
            .ensure_connected(sandbox_id)
            .await
            .map(|_| ())
    }

    async fn delete_host(
        &self,
        api_key: &str,
        sandbox_id: &str,
        timeout_seconds: u64,
    ) -> Result<(), E2bError> {
        E2bControlClient::new(self.config(api_key, timeout_seconds)?)?
            .delete_sandbox(sandbox_id)
            .await
    }

    async fn create_snapshot(
        &self,
        api_key: &str,
        sandbox_id: &str,
        timeout_seconds: u64,
    ) -> Result<String, E2bError> {
        E2bControlClient::new(self.config(api_key, timeout_seconds)?)?
            .snapshot_sandbox(sandbox_id)
            .await
    }

    async fn delete_snapshot(&self, api_key: &str, snapshot_id: &str) -> Result<(), E2bError> {
        E2bControlClient::new(self.config(api_key, 300)?)?
            .delete_snapshot(snapshot_id)
            .await
    }

    async fn execute(
        &self,
        api_key: &str,
        sandbox_id: &str,
        host_id: ExecutionHostId,
        timeout_seconds: u64,
        request: RequestEnvelope,
    ) -> Result<ProviderExecutionResponse, E2bError> {
        let runtime = E2bExecutionRuntime::connect(E2bRuntimeConfig {
            e2b: self.config(api_key, timeout_seconds)?,
            sandbox_id: sandbox_id.to_owned(),
            host_id,
            supervisor: SupervisorConfig::default(),
        })
        .await?;
        let descriptor = execution_core::ExecutionRuntime::descriptor(&runtime).clone();
        let response = dispatch_request(
            &runtime,
            &OperationContext::with_timeout(self.settings.request_timeout),
            request,
        )
        .await;
        Ok(ProviderExecutionResponse {
            descriptor,
            response,
        })
    }
}

pub type DynE2bProvider = Arc<dyn E2bProvider>;
