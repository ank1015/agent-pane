//! Executable interfaces for local and remote execution environments.
//!
//! [`execution_contracts`] owns serializable values. This crate owns the async
//! Rust traits implemented by machine daemons, SSH workers, local runtimes, and
//! provider-native sandbox adapters. It deliberately contains no transport,
//! persistence, shell, filesystem, or sandbox implementation.

pub mod artifacts;
pub mod conformance;
pub mod context;
pub mod environment;
pub mod filesystem;
pub mod process;
pub mod workspace;

pub use artifacts::{ArtifactChunkStream, ArtifactStore};
pub use context::OperationContext;
pub use environment::{
    CapabilityConsistencyError, CodeIntelligence, ExecutionEnvironment, MediaProcessing,
    validate_capability_consistency,
};
pub use filesystem::BasicFileSystem;
pub use process::{ProcessEventStream, ProcessRuntime};
pub use workspace::{WorkspaceMutation, WorkspaceQuery};

/// Result returned by an execution capability.
pub type ExecutionResult<T> = Result<T, execution_contracts::ExecutionError>;
