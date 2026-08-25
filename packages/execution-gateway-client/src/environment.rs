use execution_contracts::{
    ARTIFACTS_CAPABILITY, BASIC_FILESYSTEM_CAPABILITY, CODE_INTELLIGENCE_CAPABILITY,
    EnvironmentDescriptor, MEDIA_PROCESSING_CAPABILITY, MachineId, PROCESS_SESSION_CAPABILITY,
    Validate, WORKSPACE_MUTATION_CAPABILITY, WORKSPACE_QUERY_CAPABILITY,
};
use execution_runtime::{
    ArtifactStore, BasicFileSystem, CodeIntelligence, ExecutionEnvironment, MediaProcessing,
    ProcessRuntime, WorkspaceMutation, WorkspaceQuery, validate_capability_consistency,
};

use crate::{ExecutionGatewayClient, ExecutionGatewayClientError};

#[derive(Clone)]
pub struct GatewayExecutionEnvironment {
    pub(crate) client: ExecutionGatewayClient,
    pub(crate) descriptor: EnvironmentDescriptor,
}

impl GatewayExecutionEnvironment {
    pub async fn connect(
        client: ExecutionGatewayClient,
        machine_id: &MachineId,
    ) -> Result<Self, ExecutionGatewayClientError> {
        let machine = client.machine(machine_id).await?;
        if &machine.machine_id != machine_id || &machine.descriptor.machine_id != machine_id {
            return Err(ExecutionGatewayClientError::InvalidDescriptor(
                "gateway machine identity does not match the requested machine".to_owned(),
            ));
        }
        Self::from_descriptor(client, machine.descriptor)
    }

    pub fn from_descriptor(
        client: ExecutionGatewayClient,
        descriptor: EnvironmentDescriptor,
    ) -> Result<Self, ExecutionGatewayClientError> {
        descriptor
            .validate()
            .map_err(|error| ExecutionGatewayClientError::InvalidDescriptor(error.to_string()))?;
        if !has_capability(&descriptor, WORKSPACE_QUERY_CAPABILITY) {
            return Err(ExecutionGatewayClientError::InvalidDescriptor(
                "environment must advertise workspace.query v1".to_owned(),
            ));
        }
        let environment = Self { client, descriptor };
        validate_capability_consistency(&environment)
            .map_err(|error| ExecutionGatewayClientError::InvalidDescriptor(error.to_string()))?;
        Ok(environment)
    }

    #[must_use]
    pub fn machine_id(&self) -> &MachineId {
        &self.descriptor.machine_id
    }

    #[must_use]
    pub fn client(&self) -> &ExecutionGatewayClient {
        &self.client
    }
}

impl ExecutionEnvironment for GatewayExecutionEnvironment {
    fn descriptor(&self) -> &EnvironmentDescriptor {
        &self.descriptor
    }

    fn workspace_query(&self) -> &dyn WorkspaceQuery {
        self
    }

    fn workspace_mutation(&self) -> Option<&dyn WorkspaceMutation> {
        has_capability(&self.descriptor, WORKSPACE_MUTATION_CAPABILITY)
            .then_some(self as &dyn WorkspaceMutation)
    }

    fn process_runtime(&self) -> Option<&dyn ProcessRuntime> {
        has_capability(&self.descriptor, PROCESS_SESSION_CAPABILITY)
            .then_some(self as &dyn ProcessRuntime)
    }

    fn artifact_store(&self) -> Option<&dyn ArtifactStore> {
        has_capability(&self.descriptor, ARTIFACTS_CAPABILITY).then_some(self as &dyn ArtifactStore)
    }

    fn filesystem(&self) -> Option<&dyn BasicFileSystem> {
        has_capability(&self.descriptor, BASIC_FILESYSTEM_CAPABILITY)
            .then_some(self as &dyn BasicFileSystem)
    }

    fn code_intelligence(&self) -> Option<&dyn CodeIntelligence> {
        has_capability(&self.descriptor, CODE_INTELLIGENCE_CAPABILITY)
            .then_some(self as &dyn CodeIntelligence)
    }

    fn media_processing(&self) -> Option<&dyn MediaProcessing> {
        has_capability(&self.descriptor, MEDIA_PROCESSING_CAPABILITY)
            .then_some(self as &dyn MediaProcessing)
    }
}

impl CodeIntelligence for GatewayExecutionEnvironment {}
impl MediaProcessing for GatewayExecutionEnvironment {}

fn has_capability(descriptor: &EnvironmentDescriptor, id: &str) -> bool {
    descriptor
        .capabilities
        .iter()
        .any(|capability| capability.id.as_str() == id && capability.major == 1)
}
