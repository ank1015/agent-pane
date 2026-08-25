use std::sync::Arc;

use execution_contracts::{
    ARTIFACTS_CAPABILITY, BASIC_FILESYSTEM_CAPABILITY, Capability, MachineDescriptor,
    OperatingSystem, PROCESS_SESSION_CAPABILITY, PathConvention, ProtocolVersion, ShellDescriptor,
    WORKSPACE_MUTATION_CAPABILITY, WORKSPACE_QUERY_CAPABILITY,
};
use execution_runtime::{
    ArtifactStore, BasicFileSystem, ExecutionRuntime, ProcessRuntime, WorkspaceMutation,
    WorkspaceQuery, validate_capability_consistency,
};

use crate::{
    DaytonaConnectionConfig, DaytonaConnectorError, DaytonaHttpTransport, DaytonaRuntimeConfig,
    DaytonaTransport, artifacts::DaytonaArtifactStore, backend::DaytonaBackend,
    process::DaytonaProcessRuntime, runner::InlineRunner,
};

pub struct DaytonaExecutionRuntime {
    descriptor: MachineDescriptor,
    backend: DaytonaBackend,
    process: DaytonaProcessRuntime,
    artifacts: DaytonaArtifactStore,
}

impl DaytonaExecutionRuntime {
    pub fn connect(
        connection: DaytonaConnectionConfig,
        config: DaytonaRuntimeConfig,
    ) -> Result<Self, DaytonaConnectorError> {
        let python_command = connection.python_command.clone();
        let network_block_all = connection.network_block_all;
        let transport =
            Arc::new(DaytonaHttpTransport::new(connection)?) as Arc<dyn DaytonaTransport>;
        Self::with_transport(transport, python_command, network_block_all, config)
    }

    pub fn with_transport(
        transport: Arc<dyn DaytonaTransport>,
        python_command: String,
        network_block_all: bool,
        config: DaytonaRuntimeConfig,
    ) -> Result<Self, DaytonaConnectorError> {
        validate_config(&config, &python_command)?;
        let runner = Arc::new(InlineRunner::new(
            Arc::clone(&transport),
            python_command.clone(),
            &config,
        ));
        let backend = DaytonaBackend::new(Arc::clone(&runner));
        let artifacts = DaytonaArtifactStore::new(Arc::clone(&runner));
        let process = DaytonaProcessRuntime::new(
            transport,
            runner,
            python_command,
            network_block_all,
            &config,
        );
        let descriptor = MachineDescriptor {
            protocol_version: ProtocolVersion::V1,
            machine_id: config.machine_id,
            name: config.name,
            operating_system: OperatingSystem::Linux,
            architecture: "x86_64".to_owned(),
            path_convention: PathConvention::Posix,
            default_shell: Some(ShellDescriptor {
                name: "bash".to_owned(),
                executable: "/bin/bash".to_owned(),
            }),
            workspace_roots: config
                .workspace_roots
                .iter()
                .map(|root| root.contract())
                .collect(),
            capabilities: [
                WORKSPACE_QUERY_CAPABILITY,
                WORKSPACE_MUTATION_CAPABILITY,
                PROCESS_SESSION_CAPABILITY,
                ARTIFACTS_CAPABILITY,
                BASIC_FILESYSTEM_CAPABILITY,
            ]
            .into_iter()
            .map(|id| Capability::v1(id).expect("built-in capability ID is valid"))
            .collect(),
        };
        let runtime = Self {
            descriptor,
            backend,
            process,
            artifacts,
        };
        validate_capability_consistency(&runtime)
            .map_err(|source| DaytonaConnectorError::InvalidConfiguration(source.to_string()))?;
        Ok(runtime)
    }
}

impl ExecutionRuntime for DaytonaExecutionRuntime {
    fn descriptor(&self) -> &MachineDescriptor {
        &self.descriptor
    }

    fn workspace_query(&self) -> &dyn WorkspaceQuery {
        &self.backend
    }

    fn workspace_mutation(&self) -> Option<&dyn WorkspaceMutation> {
        Some(&self.backend)
    }

    fn process_runtime(&self) -> Option<&dyn ProcessRuntime> {
        Some(&self.process)
    }

    fn artifact_store(&self) -> Option<&dyn ArtifactStore> {
        Some(&self.artifacts)
    }

    fn filesystem(&self) -> Option<&dyn BasicFileSystem> {
        Some(&self.backend)
    }
}

fn validate_config(
    config: &DaytonaRuntimeConfig,
    python_command: &str,
) -> Result<(), DaytonaConnectorError> {
    if config.name.trim().is_empty() {
        return Err(DaytonaConnectorError::InvalidConfiguration(
            "machine name must not be empty".to_owned(),
        ));
    }
    if python_command.trim().is_empty() {
        return Err(DaytonaConnectorError::InvalidConfiguration(
            "Python command must not be empty".to_owned(),
        ));
    }
    if !config.state_directory.starts_with('/') {
        return Err(DaytonaConnectorError::InvalidConfiguration(
            "state directory must be an absolute Daytona path".to_owned(),
        ));
    }
    if config.workspace_roots.is_empty() {
        return Err(DaytonaConnectorError::InvalidConfiguration(
            "at least one workspace root is required".to_owned(),
        ));
    }
    for root in &config.workspace_roots {
        if !root.path.starts_with('/') {
            return Err(DaytonaConnectorError::InvalidConfiguration(format!(
                "workspace root `{}` must use an absolute Daytona path",
                root.id
            )));
        }
    }
    Ok(())
}
