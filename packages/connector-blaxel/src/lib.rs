//! Direct Blaxel implementation of the execution runtime.
//!
//! The connector talks to Blaxel's sandbox proxy APIs directly. Filesystem
//! semantics that the file API does not expose are implemented by short-lived
//! inline Python commands;
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

pub use config::{
    BlaxelConnectionConfig, BlaxelNativeGrant, BlaxelRuntimeConfig, BlaxelWorkspaceRoot,
};
pub use error::{BlaxelConnectorError, BlaxelTransportError};
pub use http::BlaxelHttpTransport;
pub use lifecycle::{
    ReadyBlaxelSandbox, create, create_from_snapshot, create_snapshot, resolve_workspace,
    terminate, wait_until_ready,
};
pub use runtime::BlaxelExecutionRuntime;
pub use transport::{
    BlaxelTransport, RemoteProcessEvent, RemoteProcessRequest, RemoteProcessStream,
    RemoteProcessSummary, RemoteStreamKind,
};
