mod http;
pub mod model;

use uuid::Uuid;

use crate::upstream::execution_gateway::{ExecutionGatewayClient, ExecutionGatewayError};
use execution_protocol::MachineSummary;
use model::{
    CreateSandboxAccountRequest, MachineInventory, RotateSandboxCredentialsRequest, SandboxAccount,
    UpdateNameRequest,
};

#[derive(Clone)]
pub struct MachineService {
    gateway: ExecutionGatewayClient,
}

impl MachineService {
    #[must_use]
    pub const fn new(gateway: ExecutionGatewayClient) -> Self {
        Self { gateway }
    }

    async fn machine_inventory(&self) -> Result<MachineInventory, ExecutionGatewayError> {
        self.gateway.machine_inventory().await
    }

    async fn list_sandbox_accounts(&self) -> Result<Vec<SandboxAccount>, ExecutionGatewayError> {
        self.gateway.list_sandbox_accounts().await
    }

    async fn get_sandbox_account(
        &self,
        account_id: Uuid,
    ) -> Result<SandboxAccount, ExecutionGatewayError> {
        self.gateway.get_sandbox_account(account_id).await
    }

    async fn create_sandbox_account(
        &self,
        request: &CreateSandboxAccountRequest,
    ) -> Result<SandboxAccount, ExecutionGatewayError> {
        self.gateway.create_sandbox_account(request).await
    }

    async fn rotate_sandbox_credentials(
        &self,
        account_id: Uuid,
        request: &RotateSandboxCredentialsRequest,
    ) -> Result<SandboxAccount, ExecutionGatewayError> {
        self.gateway
            .rotate_sandbox_credentials(account_id, request)
            .await
    }

    async fn update_sandbox_account_name(
        &self,
        account_id: Uuid,
        request: &UpdateNameRequest,
    ) -> Result<SandboxAccount, ExecutionGatewayError> {
        self.gateway
            .update_sandbox_account_name(account_id, request)
            .await
    }

    async fn delete_sandbox_account(&self, account_id: Uuid) -> Result<(), ExecutionGatewayError> {
        self.gateway.delete_sandbox_account(account_id).await
    }

    async fn update_machine_name(
        &self,
        machine_id: &str,
        request: &UpdateNameRequest,
    ) -> Result<MachineSummary, ExecutionGatewayError> {
        self.gateway.update_machine_name(machine_id, request).await
    }

    async fn delete_machine(&self, machine_id: &str) -> Result<(), ExecutionGatewayError> {
        self.gateway.delete_machine(machine_id).await
    }
}

pub(crate) fn router(service: MachineService) -> axum::Router<crate::AppState> {
    http::router(service)
}
