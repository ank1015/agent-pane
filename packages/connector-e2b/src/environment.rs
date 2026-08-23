use std::sync::Arc;

use execution_contracts::{
    ARTIFACTS_CAPABILITY, BASIC_FILESYSTEM_CAPABILITY, Capability, EnvironmentDescriptor,
    OperatingSystem, PROCESS_SESSION_CAPABILITY, PathConvention, ProtocolVersion, ShellDescriptor,
    WORKSPACE_MUTATION_CAPABILITY, WORKSPACE_QUERY_CAPABILITY,
};
use execution_runtime::{
    ArtifactStore, BasicFileSystem, ExecutionEnvironment, ProcessRuntime, WorkspaceMutation,
    WorkspaceQuery, validate_capability_consistency,
};

use crate::{
    E2bConnectionConfig, E2bConnectorError, E2bEnvironmentConfig, E2bHttpTransport, E2bTransport,
    artifacts::E2bArtifactStore, backend::E2bBackend, process::E2bProcessRuntime,
    runner::InlineRunner,
};

pub struct E2bExecutionEnvironment {
    descriptor: EnvironmentDescriptor,
    backend: E2bBackend,
    process: E2bProcessRuntime,
    artifacts: E2bArtifactStore,
}

impl E2bExecutionEnvironment {
    pub fn connect(
        connection: E2bConnectionConfig,
        config: E2bEnvironmentConfig,
    ) -> Result<Self, E2bConnectorError> {
        let python_command = connection.python_command.clone();
        let transport = Arc::new(E2bHttpTransport::new(connection)?) as Arc<dyn E2bTransport>;
        Self::with_transport(transport, python_command, config)
    }

    pub fn with_transport(
        transport: Arc<dyn E2bTransport>,
        python_command: String,
        config: E2bEnvironmentConfig,
    ) -> Result<Self, E2bConnectorError> {
        validate_config(&config, &python_command)?;
        let runner = Arc::new(InlineRunner::new(
            Arc::clone(&transport),
            python_command.clone(),
            &config,
        ));
        let backend = E2bBackend::new(Arc::clone(&runner));
        let artifacts = E2bArtifactStore::new(Arc::clone(&runner));
        let process = E2bProcessRuntime::new(transport, runner, python_command, &config);
        let descriptor = EnvironmentDescriptor {
            protocol_version: ProtocolVersion::V1,
            machine_id: config.machine_id,
            environment_id: config.environment_id,
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
        let environment = Self {
            descriptor,
            backend,
            process,
            artifacts,
        };
        validate_capability_consistency(&environment)
            .map_err(|source| E2bConnectorError::InvalidConfiguration(source.to_string()))?;
        Ok(environment)
    }
}

impl ExecutionEnvironment for E2bExecutionEnvironment {
    fn descriptor(&self) -> &EnvironmentDescriptor {
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
    config: &E2bEnvironmentConfig,
    python_command: &str,
) -> Result<(), E2bConnectorError> {
    if config.name.trim().is_empty() {
        return Err(E2bConnectorError::InvalidConfiguration(
            "environment name must not be empty".to_owned(),
        ));
    }
    if python_command.trim().is_empty() {
        return Err(E2bConnectorError::InvalidConfiguration(
            "Python command must not be empty".to_owned(),
        ));
    }
    if !config.state_directory.starts_with('/') {
        return Err(E2bConnectorError::InvalidConfiguration(
            "state directory must be an absolute E2B path".to_owned(),
        ));
    }
    if config.workspace_roots.is_empty() {
        return Err(E2bConnectorError::InvalidConfiguration(
            "at least one workspace root is required".to_owned(),
        ));
    }
    for root in &config.workspace_roots {
        if !root.path.starts_with('/') {
            return Err(E2bConnectorError::InvalidConfiguration(format!(
                "workspace root `{}` must use an absolute E2B path",
                root.id
            )));
        }
    }
    Ok(())
}
