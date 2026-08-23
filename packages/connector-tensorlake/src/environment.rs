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
    TensorlakeConnectionConfig, TensorlakeConnectorError, TensorlakeEnvironmentConfig,
    TensorlakeHttpTransport, TensorlakeTransport, artifacts::TensorlakeArtifactStore,
    backend::TensorlakeBackend, process::TensorlakeProcessRuntime, runner::InlineRunner,
};

pub struct TensorlakeExecutionEnvironment {
    descriptor: EnvironmentDescriptor,
    backend: TensorlakeBackend,
    process: TensorlakeProcessRuntime,
    artifacts: TensorlakeArtifactStore,
}

impl TensorlakeExecutionEnvironment {
    pub fn connect(
        connection: TensorlakeConnectionConfig,
        config: TensorlakeEnvironmentConfig,
    ) -> Result<Self, TensorlakeConnectorError> {
        let python_command = connection.python_command.clone();
        let transport =
            Arc::new(TensorlakeHttpTransport::new(connection)?) as Arc<dyn TensorlakeTransport>;
        Self::with_transport(transport, python_command, config)
    }

    pub fn with_transport(
        transport: Arc<dyn TensorlakeTransport>,
        python_command: String,
        config: TensorlakeEnvironmentConfig,
    ) -> Result<Self, TensorlakeConnectorError> {
        validate_config(&config, &python_command)?;
        let runner = Arc::new(InlineRunner::new(
            Arc::clone(&transport),
            python_command.clone(),
            &config,
        ));
        let backend = TensorlakeBackend::new(Arc::clone(&runner));
        let artifacts = TensorlakeArtifactStore::new(Arc::clone(&runner));
        let process = TensorlakeProcessRuntime::new(transport, runner, python_command, &config);
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
            .map_err(|source| TensorlakeConnectorError::InvalidConfiguration(source.to_string()))?;
        Ok(environment)
    }
}

impl ExecutionEnvironment for TensorlakeExecutionEnvironment {
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
    config: &TensorlakeEnvironmentConfig,
    python_command: &str,
) -> Result<(), TensorlakeConnectorError> {
    if config.name.trim().is_empty() {
        return Err(TensorlakeConnectorError::InvalidConfiguration(
            "environment name must not be empty".to_owned(),
        ));
    }
    if python_command.trim().is_empty() {
        return Err(TensorlakeConnectorError::InvalidConfiguration(
            "Python command must not be empty".to_owned(),
        ));
    }
    if !config.state_directory.starts_with('/') {
        return Err(TensorlakeConnectorError::InvalidConfiguration(
            "state directory must be an absolute Tensorlake path".to_owned(),
        ));
    }
    if config.workspace_roots.is_empty() {
        return Err(TensorlakeConnectorError::InvalidConfiguration(
            "at least one workspace root is required".to_owned(),
        ));
    }
    for root in &config.workspace_roots {
        if !root.path.starts_with('/') {
            return Err(TensorlakeConnectorError::InvalidConfiguration(format!(
                "workspace root `{}` must use an absolute Tensorlake path",
                root.id
            )));
        }
    }
    Ok(())
}
