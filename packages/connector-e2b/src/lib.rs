//! Direct E2B implementation of the execution runtime.
//!
//! The connector talks to E2B's envd APIs directly. Filesystem semantics that
//! envd does not expose are implemented by short-lived inline Python commands;
//! no resident agent-pane daemon is installed in the sandbox.

mod artifacts;
mod backend;
mod config;
mod error;
mod http;
mod lifecycle;
mod process;
mod runner;
mod runtime;
mod transport;

pub use config::{E2bConnectionConfig, E2bNativeGrant, E2bRuntimeConfig, E2bWorkspaceRoot};
pub use error::{E2bConnectorError, E2bTransportError};
pub use http::E2bHttpTransport;
pub use lifecycle::{
    CreatedE2bSandbox, create_from_snapshot, create_from_snapshot_details, terminate,
};
pub use runtime::E2bExecutionRuntime;
pub use transport::{
    E2bTransport, RemoteProcessEvent, RemoteProcessRequest, RemoteProcessStream,
    RemoteProcessSummary, RemoteStreamKind,
};
