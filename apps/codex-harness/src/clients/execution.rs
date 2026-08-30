use std::sync::Arc;

use execution_contracts::MachineId;
use execution_gateway_client::{
    ExecutionGatewayClient, ExecutionGatewayClientError, ExecutionGatewayConfig,
    GatewayMachineRuntime,
};
use execution_runtime::ExecutionRuntime;

/// Resolves the machine runtime selected by the Agent run configuration.
#[derive(Clone)]
pub struct ExecutionClient {
    gateway: ExecutionGatewayClient,
}

impl ExecutionClient {
    pub fn new(config: ExecutionGatewayConfig) -> Result<Self, ExecutionGatewayClientError> {
        Ok(Self {
            gateway: ExecutionGatewayClient::new(config)?,
        })
    }

    pub async fn machine(
        &self,
        machine_id: &MachineId,
    ) -> Result<GatewayMachineRuntime, ExecutionGatewayClientError> {
        self.gateway.machine_runtime(machine_id).await
    }
}

#[async_trait::async_trait]
pub trait MachineRuntimeResolver: Send + Sync {
    async fn machine(
        &self,
        machine_id: &MachineId,
    ) -> Result<Arc<dyn ExecutionRuntime>, ExecutionResolutionError>;
}

#[async_trait::async_trait]
impl MachineRuntimeResolver for ExecutionClient {
    async fn machine(
        &self,
        machine_id: &MachineId,
    ) -> Result<Arc<dyn ExecutionRuntime>, ExecutionResolutionError> {
        ExecutionClient::machine(self, machine_id)
            .await
            .map(|runtime| Arc::new(runtime) as Arc<dyn ExecutionRuntime>)
            .map_err(Into::into)
    }
}

#[derive(Debug, thiserror::Error)]
pub enum ExecutionResolutionError {
    #[error("execution gateway could not resolve the selected machine")]
    Gateway(#[from] ExecutionGatewayClientError),
    #[error("selected machine could not be resolved: {0}")]
    Other(String),
}
