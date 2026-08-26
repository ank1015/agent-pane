mod http;
pub mod model;

use uuid::Uuid;

use crate::upstream::execution_gateway::{ExecutionGatewayClient, ExecutionGatewayError};
use execution_protocol::{Environment, MachineSummary};
use model::{
    CreateMachineEnvironmentRequest, CreateSandboxAccountRequest,
    CreateSandboxEnvironmentTemplateRequest, CreateSandboxRequest, CreateSandboxSnapshotRequest,
    CreateSnapshotRequest, MachineInventory, RotateSandboxCredentialsRequest, SandboxAccount,
    SandboxCreated, SandboxEnvironmentInstance, SandboxEnvironmentTemplate, SandboxMachine,
    Snapshot, SnapshotQuery, UpdateNameRequest, UpdateSandboxEnvironmentTemplateRequest,
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

    async fn list_environments(
        &self,
        machine_id: &str,
    ) -> Result<Vec<Environment>, ExecutionGatewayError> {
        self.gateway.list_environments(machine_id).await
    }

    async fn create_environment(
        &self,
        machine_id: &str,
        request: &CreateMachineEnvironmentRequest,
    ) -> Result<Environment, ExecutionGatewayError> {
        self.gateway.create_environment(machine_id, request).await
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

    async fn list_sandbox_machines(
        &self,
        account_id: Uuid,
    ) -> Result<Vec<SandboxMachine>, ExecutionGatewayError> {
        self.gateway.list_sandbox_machines(account_id).await
    }

    async fn create_sandbox(
        &self,
        account_id: Uuid,
        request: &CreateSandboxRequest,
    ) -> Result<SandboxCreated, ExecutionGatewayError> {
        self.gateway.create_sandbox(account_id, request).await
    }

    async fn create_sandbox_snapshot(
        &self,
        account_id: Uuid,
        sandbox_id: &str,
        request: &CreateSandboxSnapshotRequest,
    ) -> Result<Snapshot, ExecutionGatewayError> {
        self.gateway
            .create_sandbox_snapshot(account_id, sandbox_id, request)
            .await
    }

    async fn list_snapshots(
        &self,
        query: &SnapshotQuery,
    ) -> Result<Vec<Snapshot>, ExecutionGatewayError> {
        self.gateway.list_snapshots(query).await
    }

    async fn get_snapshot(&self, snapshot_id: Uuid) -> Result<Snapshot, ExecutionGatewayError> {
        self.gateway.get_snapshot(snapshot_id).await
    }

    async fn update_snapshot_name(
        &self,
        snapshot_id: Uuid,
        request: &UpdateNameRequest,
    ) -> Result<Snapshot, ExecutionGatewayError> {
        self.gateway
            .update_snapshot_name(snapshot_id, request)
            .await
    }

    async fn create_snapshot(
        &self,
        request: &CreateSnapshotRequest,
    ) -> Result<Snapshot, ExecutionGatewayError> {
        self.gateway.create_snapshot(request).await
    }

    async fn delete_snapshot(&self, snapshot_id: Uuid) -> Result<(), ExecutionGatewayError> {
        self.gateway.delete_snapshot(snapshot_id).await
    }

    async fn list_sandbox_environment_templates(
        &self,
    ) -> Result<Vec<SandboxEnvironmentTemplate>, ExecutionGatewayError> {
        self.gateway.list_sandbox_environment_templates().await
    }

    async fn get_sandbox_environment_template(
        &self,
        template_id: Uuid,
    ) -> Result<SandboxEnvironmentTemplate, ExecutionGatewayError> {
        self.gateway
            .get_sandbox_environment_template(template_id)
            .await
    }

    async fn create_sandbox_environment_template(
        &self,
        request: &CreateSandboxEnvironmentTemplateRequest,
    ) -> Result<SandboxEnvironmentTemplate, ExecutionGatewayError> {
        self.gateway
            .create_sandbox_environment_template(request)
            .await
    }

    async fn update_sandbox_environment_template(
        &self,
        template_id: Uuid,
        request: &UpdateSandboxEnvironmentTemplateRequest,
    ) -> Result<SandboxEnvironmentTemplate, ExecutionGatewayError> {
        self.gateway
            .update_sandbox_environment_template(template_id, request)
            .await
    }

    async fn delete_sandbox_environment_template(
        &self,
        template_id: Uuid,
    ) -> Result<(), ExecutionGatewayError> {
        self.gateway
            .delete_sandbox_environment_template(template_id)
            .await
    }

    async fn materialize_sandbox_environment_template(
        &self,
        template_id: Uuid,
    ) -> Result<SandboxEnvironmentInstance, ExecutionGatewayError> {
        self.gateway
            .materialize_sandbox_environment_template(template_id)
            .await
    }

    async fn list_sandbox_environment_instances(
        &self,
        template_id: Uuid,
    ) -> Result<Vec<SandboxEnvironmentInstance>, ExecutionGatewayError> {
        self.gateway
            .list_sandbox_environment_instances(template_id)
            .await
    }

    async fn delete_environment(&self, environment_id: &str) -> Result<(), ExecutionGatewayError> {
        self.gateway.delete_environment(environment_id).await
    }

    async fn update_environment_name(
        &self,
        environment_id: &str,
        request: &UpdateNameRequest,
    ) -> Result<Environment, ExecutionGatewayError> {
        self.gateway
            .update_environment_name(environment_id, request)
            .await
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
