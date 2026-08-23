//! Direct Tensorlake implementation of the execution runtime.
//!
//! The connector talks to Tensorlake's sandbox proxy APIs directly. Filesystem
//! semantics that the file API does not expose are implemented by short-lived
//! inline Python commands;
//! no resident agent-pane daemon is installed in the sandbox.

mod artifacts;
mod backend;
mod config;
mod environment;
mod error;
mod http;
mod process;
mod runner;
mod transport;

pub use config::{
    TensorlakeConnectionConfig, TensorlakeEnvironmentConfig, TensorlakeNativeGrant,
    TensorlakeWorkspaceRoot,
};
pub use environment::TensorlakeExecutionEnvironment;
pub use error::{TensorlakeConnectorError, TensorlakeTransportError};
pub use http::TensorlakeHttpTransport;
pub use transport::{
    RemoteProcessEvent, RemoteProcessRequest, RemoteProcessStream, RemoteProcessSummary,
    RemoteStreamKind, TensorlakeTransport,
};
