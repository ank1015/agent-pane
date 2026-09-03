//! E2B-specific lifecycle and transport for the provider-neutral execution system.
//!
//! The package does not implement filesystem or process semantics. It resumes an
//! E2B sandbox, ensures `execution-supervisor` is available, and relays the
//! versioned execution wire protocol through short-lived E2B envd processes.

mod config;
mod control;
mod envd;
mod error;
mod runtime;

pub use config::{
    E2bApiKey, E2bConfig, E2bRuntimeConfig, RetryPolicy, SupervisorBinary, SupervisorConfig,
};
pub use control::{
    ConnectedSandbox, E2bControlClient, EXECUTION_BASE_TEMPLATE_ID, SandboxDetails, SandboxState,
    create_base_sandbox, create_snapshot_sandbox, snapshot_sandbox,
};
pub use envd::{E2bEnvdClient, EnvdProcessOutput, EnvdProcessRequest};
pub use error::{E2bError, E2bErrorKind, E2bResult};
pub use runtime::E2bExecutionRuntime;
