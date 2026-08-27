use execution_contracts::MachineId;
use execution_gateway_client::{
    ExecutionGatewayClient, ExecutionGatewayClientError, ExecutionGatewayConfig,
    GatewayMachineRuntime,
};

/// Connects Pi to the resolved machine selected by its caller.
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
