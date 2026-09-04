use execution_api::{E2bAccount, ExecutionHost};

use super::{CreateE2bAccountInput, ExecutionGatewayClient, UpdateMachineInput};
use crate::error::ApiError;
use uuid::Uuid;

#[derive(Clone)]
pub struct MachineService {
    execution_gateway: ExecutionGatewayClient,
}

impl MachineService {
    #[must_use]
    pub fn new(execution_gateway: ExecutionGatewayClient) -> Self {
        Self { execution_gateway }
    }

    pub async fn list_machines(&self) -> Result<Vec<ExecutionHost>, ApiError> {
        self.execution_gateway.list_hosts().await
    }

    pub async fn list_e2b_accounts(&self) -> Result<Vec<E2bAccount>, ApiError> {
        self.execution_gateway.list_e2b_accounts().await
    }

    pub async fn create_e2b_account(
        &self,
        request: CreateE2bAccountInput,
    ) -> Result<E2bAccount, ApiError> {
        request.validate()?;
        self.execution_gateway.create_e2b_account(&request).await
    }

    pub async fn update_machine(
        &self,
        id: Uuid,
        request: UpdateMachineInput,
    ) -> Result<ExecutionHost, ApiError> {
        request.validate()?;
        self.execution_gateway.update_host(id, &request).await
    }

    pub async fn delete_machine(&self, id: Uuid) -> Result<ExecutionHost, ApiError> {
        self.execution_gateway.delete_host(id).await
    }
}
