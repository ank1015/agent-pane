use std::{path::PathBuf, sync::Arc};

use execution_contracts::{
    ARTIFACTS_CAPABILITY, BASIC_FILESYSTEM_CAPABILITY, Capability, MachineDescriptor, MachineId,
    OperatingSystem, PROCESS_SESSION_CAPABILITY, PathConvention, ProtocolVersion,
    WORKSPACE_MUTATION_CAPABILITY, WORKSPACE_QUERY_CAPABILITY, WorkspaceRootId,
};
use execution_runtime::{
    ArtifactStore, BasicFileSystem, ExecutionRuntime, ProcessRuntime, WorkspaceMutation,
    WorkspaceQuery, validate_capability_consistency,
};

use crate::{
    artifacts::LocalArtifactStore,
    error::LocalExecutionError,
    filesystem::LocalFileSystem,
    mutation::LocalWorkspaceMutation,
    path_resolver::{LocalNativeGrant, PathResolver},
    platform,
    process::LocalProcessRuntime,
    query::LocalWorkspaceQuery,
};

#[derive(Clone, Debug)]
pub struct LocalWorkspaceRoot {
    pub id: WorkspaceRootId,
    pub name: String,
    pub path: PathBuf,
    pub read_only: bool,
}

#[derive(Clone, Debug)]
pub struct LocalRuntimeConfig {
    pub machine_id: MachineId,
    pub name: String,
    pub state_directory: PathBuf,
    pub workspace_roots: Vec<LocalWorkspaceRoot>,
    pub native_grants: Vec<LocalNativeGrant>,
}

pub struct LocalExecutionRuntime {
    descriptor: MachineDescriptor,
    query: LocalWorkspaceQuery,
    mutation: LocalWorkspaceMutation,
    process: LocalProcessRuntime,
    artifacts: Arc<LocalArtifactStore>,
    filesystem: LocalFileSystem,
}

impl LocalExecutionRuntime {
    pub async fn new(config: LocalRuntimeConfig) -> Result<Self, LocalExecutionError> {
        if config.name.trim().is_empty() {
            return Err(LocalExecutionError::InvalidConfiguration(
                "machine name must not be empty".to_owned(),
            ));
        }
        tokio::fs::create_dir_all(&config.state_directory).await?;
        let resolver =
            Arc::new(PathResolver::new(config.workspace_roots, config.native_grants).await?);
        let artifacts =
            Arc::new(LocalArtifactStore::new(config.state_directory.join("artifacts")).await?);
        let filesystem = LocalFileSystem::new(Arc::clone(&resolver), Arc::clone(&artifacts));
        let query = LocalWorkspaceQuery::new(Arc::clone(&resolver), Arc::clone(&artifacts));
        let mutation = LocalWorkspaceMutation::new(
            Arc::clone(&resolver),
            filesystem.clone(),
            config.state_directory.join("mutations"),
        )
        .await?;
        let process = LocalProcessRuntime::new(Arc::clone(&resolver), Arc::clone(&artifacts));

        let descriptor = MachineDescriptor {
            protocol_version: ProtocolVersion::V1,
            machine_id: config.machine_id,
            name: config.name,
            operating_system: local_operating_system(),
            architecture: std::env::consts::ARCH.to_owned(),
            path_convention: local_path_convention(),
            default_shell: Some(platform::default_shell()),
            workspace_roots: resolver.workspace_roots(),
            capabilities: [
                WORKSPACE_QUERY_CAPABILITY,
                WORKSPACE_MUTATION_CAPABILITY,
                PROCESS_SESSION_CAPABILITY,
                ARTIFACTS_CAPABILITY,
                BASIC_FILESYSTEM_CAPABILITY,
            ]
            .into_iter()
            .map(|id| Capability::v1(id).expect("built-in capability identifier is valid"))
            .collect(),
        };

        let runtime = Self {
            descriptor,
            query,
            mutation,
            process,
            artifacts,
            filesystem,
        };
        validate_capability_consistency(&runtime)
            .map_err(|source| LocalExecutionError::InvalidConfiguration(source.to_string()))?;
        Ok(runtime)
    }
}

impl ExecutionRuntime for LocalExecutionRuntime {
    fn descriptor(&self) -> &MachineDescriptor {
        &self.descriptor
    }

    fn workspace_query(&self) -> &dyn WorkspaceQuery {
        &self.query
    }

    fn workspace_mutation(&self) -> Option<&dyn WorkspaceMutation> {
        Some(&self.mutation)
    }

    fn process_runtime(&self) -> Option<&dyn ProcessRuntime> {
        Some(&self.process)
    }

    fn artifact_store(&self) -> Option<&dyn ArtifactStore> {
        Some(self.artifacts.as_ref())
    }

    fn filesystem(&self) -> Option<&dyn BasicFileSystem> {
        Some(&self.filesystem)
    }
}

fn local_operating_system() -> OperatingSystem {
    match std::env::consts::OS {
        "linux" => OperatingSystem::Linux,
        "macos" => OperatingSystem::Macos,
        "windows" => OperatingSystem::Windows,
        "freebsd" => OperatingSystem::FreeBsd,
        name => OperatingSystem::Other {
            name: name.to_owned(),
        },
    }
}

const fn local_path_convention() -> PathConvention {
    if cfg!(windows) {
        PathConvention::Windows
    } else {
        PathConvention::Posix
    }
}
