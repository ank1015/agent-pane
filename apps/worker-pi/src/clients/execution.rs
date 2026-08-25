use execution_contracts::MachineId;
use execution_gateway_client::{
    ExecutionGatewayClient, ExecutionGatewayClientError, ExecutionGatewayConfig,
    GatewayExecutionEnvironment,
};

/// Connects claimed machine IDs to the HTTP-backed execution environment used
/// by the Pi tools.
#[derive(Clone)]
pub struct ExecutionEnvironmentClient {
    gateway: ExecutionGatewayClient,
}

impl ExecutionEnvironmentClient {
    pub fn new(config: ExecutionGatewayConfig) -> Result<Self, ExecutionGatewayClientError> {
        Ok(Self {
            gateway: ExecutionGatewayClient::new(config)?,
        })
    }

    pub async fn environment(
        &self,
        machine_id: &MachineId,
    ) -> Result<GatewayExecutionEnvironment, ExecutionGatewayClientError> {
        self.gateway.environment(machine_id).await
    }
}
