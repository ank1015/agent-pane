use std::{path::PathBuf, time::Duration};

use execution_core::{ExecutionHostId, RootId};

/// One local directory exposed through the execution filesystem.
#[derive(Clone, Debug)]
pub struct SupervisorRoot {
    pub id: RootId,
    pub name: String,
    pub path: PathBuf,
    pub read_only: bool,
}

/// Resource and retention limits enforced by a supervisor instance.
#[derive(Clone, Debug)]
pub struct SupervisorLimits {
    pub max_read_bytes: u64,
    pub max_write_bytes: u64,
    pub max_process_read_bytes: u64,
    pub max_process_input_bytes: u64,
    pub max_concurrent_processes: u32,
    pub process_output_chunk_bytes: usize,
    pub completed_execution_retention: Duration,
    pub termination_grace_period: Duration,
}

impl Default for SupervisorLimits {
    fn default() -> Self {
        Self {
            max_read_bytes: 16 * 1024 * 1024,
            max_write_bytes: 16 * 1024 * 1024,
            max_process_read_bytes: 4 * 1024 * 1024,
            max_process_input_bytes: 1024 * 1024,
            max_concurrent_processes: 64,
            process_output_chunk_bytes: 16 * 1024,
            completed_execution_retention: Duration::from_secs(15 * 60),
            termination_grace_period: Duration::from_secs(2),
        }
    }
}

/// Configuration used to create one supervisor generation.
#[derive(Clone, Debug)]
pub struct SupervisorConfig {
    pub host_id: ExecutionHostId,
    pub state_directory: PathBuf,
    pub roots: Vec<SupervisorRoot>,
    pub limits: SupervisorLimits,
}
