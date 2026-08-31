use execution_contracts::MachineId;
use execution_gateway_client::{
    ExecutionGatewayClient, ExecutionGatewayClientError, ExecutionGatewayConfig,
    GatewayMachineRuntime,
};
use execution_protocol::{
    CreateEnvironmentRequest, CreateSandboxMachineRequest, CreateSandboxTemplateEnvironmentRequest,
    Environment, MachineSummary, ProjectEnvironment, SandboxAccountSummary, SandboxMachineCreated,
    SandboxSnapshotCreated, UpdateEnvironmentRequest, UpdateSandboxTemplateEnvironmentRequest,
};
use uuid::Uuid;

/// Connects Environment to the resolved machine selected by its caller.
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

    pub async fn machines(&self) -> Result<Vec<MachineSummary>, ExecutionGatewayClientError> {
        self.gateway.list_machines().await
    }

    pub async fn machine_summary(
        &self,
        machine_id: &MachineId,
    ) -> Result<MachineSummary, ExecutionGatewayClientError> {
        self.gateway.machine(machine_id).await
    }

    pub async fn sandbox_accounts(
        &self,
    ) -> Result<Vec<SandboxAccountSummary>, ExecutionGatewayClientError> {
        self.gateway.list_sandbox_accounts().await
    }

    pub async fn create_sandbox(
        &self,
        account_id: &Uuid,
        snapshot_id: Option<Uuid>,
    ) -> Result<SandboxMachineCreated, ExecutionGatewayClientError> {
        self.gateway
            .create_sandbox(
                &account_id.to_string(),
                &CreateSandboxMachineRequest { snapshot_id },
            )
            .await
    }

    pub async fn snapshot_sandbox(
        &self,
        machine_id: &MachineId,
    ) -> Result<SandboxSnapshotCreated, ExecutionGatewayClientError> {
        self.gateway.snapshot_sandbox(machine_id).await
    }

    pub async fn create_environment(
        &self,
        request: &CreateEnvironmentRequest,
    ) -> Result<Environment, ExecutionGatewayClientError> {
        self.gateway.create_environment(request).await
    }

    pub async fn create_sandbox_template_environment(
        &self,
        request: &CreateSandboxTemplateEnvironmentRequest,
    ) -> Result<ProjectEnvironment, ExecutionGatewayClientError> {
        self.gateway
            .create_sandbox_template_environment(request)
            .await
    }

    pub async fn update_environment(
        &self,
        environment_id: &str,
        request: &UpdateEnvironmentRequest,
    ) -> Result<ProjectEnvironment, ExecutionGatewayClientError> {
        self.gateway
            .update_environment(environment_id, request)
            .await
    }

    pub async fn update_sandbox_template_environment(
        &self,
        template_id: &str,
        request: &UpdateSandboxTemplateEnvironmentRequest,
    ) -> Result<ProjectEnvironment, ExecutionGatewayClientError> {
        self.gateway
            .update_sandbox_template_environment(template_id, request)
            .await
    }

    pub async fn project_environments(
        &self,
        project_id: &Uuid,
    ) -> Result<Vec<ProjectEnvironment>, ExecutionGatewayClientError> {
        self.gateway
            .list_project_environments(&project_id.to_string())
            .await
    }
}
