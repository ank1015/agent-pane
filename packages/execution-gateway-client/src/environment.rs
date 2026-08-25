use execution_contracts::{
    EnvironmentId, MachineDescriptor, MachineId, PathSpec, Validate, WorkspaceRootId,
};
use execution_protocol::Environment;
use execution_runtime::{
    ArtifactStore, BasicFileSystem, CodeIntelligence, ExecutionRuntime, MediaProcessing,
    ProcessRuntime, WorkspaceMutation, WorkspaceQuery,
};

use crate::{ExecutionGatewayClient, ExecutionGatewayClientError, GatewayMachineRuntime};

/// An HTTP-backed execution runtime anchored at a saved working location.
pub struct GatewayEnvironment {
    environment: Environment,
    runtime: GatewayMachineRuntime,
}

impl GatewayEnvironment {
    pub async fn connect(
        client: ExecutionGatewayClient,
        environment_id: &EnvironmentId,
    ) -> Result<Self, ExecutionGatewayClientError> {
        let environment = client.environment_record(environment_id).await?;
        if &environment.environment_id != environment_id {
            return Err(ExecutionGatewayClientError::InvalidEnvironment(
                "gateway environment identity does not match the requested environment".to_owned(),
            ));
        }
        PathSpec::workspace(
            environment.workspace_root_id.clone(),
            environment.path.clone(),
        )
        .validate()
        .map_err(|error| ExecutionGatewayClientError::InvalidEnvironment(error.to_string()))?;
        let machine = client.machine(&environment.machine_id).await?;
        if machine.machine_id != environment.machine_id
            || machine.descriptor.machine_id != environment.machine_id
        {
            return Err(ExecutionGatewayClientError::InvalidEnvironment(
                "gateway environment machine identity is inconsistent".to_owned(),
            ));
        }
        if !machine
            .descriptor
            .workspace_roots
            .iter()
            .any(|root| root.id == environment.workspace_root_id)
        {
            return Err(ExecutionGatewayClientError::InvalidEnvironment(
                "gateway environment workspace root is not exposed by its machine".to_owned(),
            ));
        }
        let runtime = GatewayMachineRuntime::for_environment(
            client,
            machine.descriptor,
            environment.environment_id.clone(),
        )?;
        Ok(Self {
            environment,
            runtime,
        })
    }

    #[must_use]
    pub fn environment_id(&self) -> &EnvironmentId {
        &self.environment.environment_id
    }

    #[must_use]
    pub fn machine_id(&self) -> &MachineId {
        &self.environment.machine_id
    }

    #[must_use]
    pub fn workspace_root_id(&self) -> &WorkspaceRootId {
        &self.environment.workspace_root_id
    }

    #[must_use]
    pub fn path(&self) -> &str {
        &self.environment.path
    }

    #[must_use]
    pub fn record(&self) -> &Environment {
        &self.environment
    }
}

impl ExecutionRuntime for GatewayEnvironment {
    fn descriptor(&self) -> &MachineDescriptor {
        self.runtime.descriptor()
    }

    fn workspace_query(&self) -> &dyn WorkspaceQuery {
        self.runtime.workspace_query()
    }

    fn workspace_mutation(&self) -> Option<&dyn WorkspaceMutation> {
        self.runtime.workspace_mutation()
    }

    fn process_runtime(&self) -> Option<&dyn ProcessRuntime> {
        self.runtime.process_runtime()
    }

    fn artifact_store(&self) -> Option<&dyn ArtifactStore> {
        self.runtime.artifact_store()
    }

    fn filesystem(&self) -> Option<&dyn BasicFileSystem> {
        self.runtime.filesystem()
    }

    fn code_intelligence(&self) -> Option<&dyn CodeIntelligence> {
        self.runtime.code_intelligence()
    }

    fn media_processing(&self) -> Option<&dyn MediaProcessing> {
        self.runtime.media_processing()
    }
}
