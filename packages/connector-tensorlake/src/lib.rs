//! Direct Tensorlake implementation of the execution runtime.
//!
//! The connector talks to Tensorlake's sandbox proxy APIs directly. Filesystem
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
    TensorlakeConnectionConfig, TensorlakeNativeGrant, TensorlakeRuntimeConfig,
    TensorlakeWorkspaceRoot,
};
pub use error::{TensorlakeConnectorError, TensorlakeTransportError};
pub use http::TensorlakeHttpTransport;
pub use lifecycle::{ReadyTensorlakeSandbox, create_from_snapshot, terminate, wait_until_ready};
pub use runtime::TensorlakeExecutionRuntime;
pub use transport::{
    RemoteProcessEvent, RemoteProcessRequest, RemoteProcessStream, RemoteProcessSummary,
    RemoteStreamKind, TensorlakeTransport,
};
