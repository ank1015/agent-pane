//! Direct Blaxel implementation of the execution runtime.
//!
//! The connector talks to Blaxel's sandbox proxy APIs directly. Filesystem
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
    BlaxelConnectionConfig, BlaxelEnvironmentConfig, BlaxelNativeGrant, BlaxelWorkspaceRoot,
};
pub use environment::BlaxelExecutionEnvironment;
pub use error::{BlaxelConnectorError, BlaxelTransportError};
pub use http::BlaxelHttpTransport;
pub use transport::{
    BlaxelTransport, RemoteProcessEvent, RemoteProcessRequest, RemoteProcessStream,
    RemoteProcessSummary, RemoteStreamKind,
};
