use std::{path::PathBuf, sync::Arc};

use execution_core::{
    ExecutionFeatures, ExecutionHostDescriptor, ExecutionRuntime, OperatingSystem, PathConvention,
    SupervisorGenerationId, Validate,
};

use crate::{
    SupervisorConfig, SupervisorInitError, filesystem::SupervisorFileSystem,
    path_resolver::PathResolver, process::SupervisorProcessRuntime,
};

/// A single local supervisor generation serving filesystem and process operations.
pub struct SupervisorRuntime {
    descriptor: ExecutionHostDescriptor,
    filesystem: SupervisorFileSystem,
    processes: SupervisorProcessRuntime,
    generation_state_directory: PathBuf,
}

impl SupervisorRuntime {
    /// Creates a new supervisor generation from local roots and resource limits.
    pub async fn new(config: SupervisorConfig) -> Result<Self, SupervisorInitError> {
        validate_config(&config)?;

        let generation_id = SupervisorGenerationId::generate();
        let generation_state_directory = config
            .state_directory
            .join("generations")
            .join(generation_id.as_str());
        tokio::fs::create_dir_all(&generation_state_directory).await?;

        let resolver = Arc::new(PathResolver::new(config.roots).await?);
        let filesystem = SupervisorFileSystem::new(Arc::clone(&resolver), config.limits.clone());
        let descriptor = ExecutionHostDescriptor {
            host_id: config.host_id,
            supervisor_generation_id: generation_id.clone(),
            operating_system: host_operating_system(),
            architecture: std::env::consts::ARCH.to_string(),
            path_convention: if cfg!(windows) {
                PathConvention::Windows
            } else {
                PathConvention::Unix
            },
            roots: resolver.descriptors(),
            features: ExecutionFeatures {
                pty: true,
                process_signals: cfg!(any(unix, windows)),
                file_revisions: true,
            },
            limits: filesystem.descriptor_limits(),
        };
        descriptor.validate().map_err(|source| {
            SupervisorInitError::InvalidConfiguration(format!(
                "generated execution descriptor is invalid: {source}"
            ))
        })?;

        let processes = SupervisorProcessRuntime::new(
            resolver,
            generation_id,
            &generation_state_directory,
            config.limits,
        )
        .await?;

        let runtime = Self {
            descriptor,
            filesystem,
            processes,
            generation_state_directory,
        };
        runtime.persist_descriptor().await?;
        Ok(runtime)
    }

    /// Stops every process still owned by this generation.
    pub async fn shutdown(&self) {
        self.processes.shutdown().await;
    }

    /// Private state directory allocated to this supervisor generation.
    #[must_use]
    pub fn generation_state_directory(&self) -> &std::path::Path {
        &self.generation_state_directory
    }

    async fn persist_descriptor(&self) -> Result<(), SupervisorInitError> {
        let bytes = serde_json::to_vec_pretty(&self.descriptor).map_err(|source| {
            SupervisorInitError::InvalidConfiguration(format!(
                "failed to serialize execution descriptor: {source}"
            ))
        })?;
        tokio::fs::write(
            self.generation_state_directory.join("descriptor.json"),
            bytes,
        )
        .await?;
        Ok(())
    }
}

impl ExecutionRuntime for SupervisorRuntime {
    fn descriptor(&self) -> &ExecutionHostDescriptor {
        &self.descriptor
    }

    fn filesystem(&self) -> &dyn execution_core::FileSystem {
        &self.filesystem
    }

    fn processes(&self) -> &dyn execution_core::ProcessRuntime {
        &self.processes
    }
}

fn validate_config(config: &SupervisorConfig) -> Result<(), SupervisorInitError> {
    let limits = &config.limits;
    if limits.max_read_bytes == 0
        || limits.max_write_bytes == 0
        || limits.max_process_read_bytes == 0
        || limits.max_process_input_bytes == 0
        || limits.max_concurrent_processes == 0
        || limits.process_output_chunk_bytes == 0
        || limits.completed_execution_retention.is_zero()
    {
        return Err(SupervisorInitError::InvalidConfiguration(
            "all supervisor byte, process, chunk, and retention limits must be greater than zero"
                .to_string(),
        ));
    }
    if config.state_directory.as_os_str().is_empty() {
        return Err(SupervisorInitError::InvalidConfiguration(
            "state_directory must not be empty".to_string(),
        ));
    }
    Ok(())
}

fn host_operating_system() -> OperatingSystem {
    match std::env::consts::OS {
        "linux" => OperatingSystem::Linux,
        "macos" => OperatingSystem::Macos,
        "windows" => OperatingSystem::Windows,
        other => OperatingSystem::Other(other.to_string()),
    }
}
