//! Provider-neutral semantics for filesystem access and durable process sessions.
//!
//! This crate defines the values and object-safe async traits shared by execution
//! clients, supervisors, provider connectors, and hosted routing. It contains no
//! transport, persistence, provider, or operating-system implementation.

mod context;
mod data;
mod descriptor;
mod error;
mod filesystem;
mod ids;
mod process;
mod runtime;
mod validation;

pub use context::OperationContext;
pub use data::{BinaryData, TimestampMs};
pub use descriptor::{
    ExecutionFeatures, ExecutionHostDescriptor, ExecutionLimits, ExecutionRoot, OperatingSystem,
    PathConvention,
};
pub use error::{ExecutionError, ExecutionErrorCode, ExecutionResult};
pub use filesystem::{
    CreateDirectoryRequest, DirectoryEntry, ExecutionPath, FileKind, FileMetadata, FileSystem,
    ListDirectoryRequest, ListDirectoryResult, ReadFileRequest, ReadFileResult, RemovePathRequest,
    RemovePathResult, RemoveTargetKind, StatRequest, WriteCondition, WriteFileRequest,
    WriteFileResult, WriteStrategy,
};
pub use ids::{
    DirectoryCursor, ExecutionHostId, ExecutionId, FileRevision, OperationId, RootId,
    SupervisorGenerationId, WriteId,
};
pub use process::{
    CommandSpec, EnvironmentMode, EnvironmentVariables, ExecutionHandle, ExecutionState,
    ProcessEvent, ProcessEventKind, ProcessInput, ProcessInputStatus, ProcessOutputStream,
    ProcessRuntime, ProcessSignal, ReadExecutionRequest, ReadExecutionResult, ResizePtyRequest,
    SignalExecutionRequest, StartExecutionRequest, StdinMode, TerminateExecutionRequest,
    TerminateExecutionResult, WriteProcessInputRequest, WriteProcessInputResult,
};
pub use runtime::ExecutionRuntime;
pub use validation::{Validate, ValidationError, ValidationIssue};
