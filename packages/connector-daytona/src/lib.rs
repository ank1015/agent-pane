//! Direct Daytona implementation of the execution runtime.
//!
//! The connector talks to Daytona's sandbox proxy APIs directly. Filesystem
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
    DaytonaConnectionConfig, DaytonaEnvironmentConfig, DaytonaNativeGrant, DaytonaWorkspaceRoot,
};
pub use environment::DaytonaExecutionEnvironment;
pub use error::{DaytonaConnectorError, DaytonaTransportError};
pub use http::DaytonaHttpTransport;
pub use transport::{
    DaytonaTransport, RemoteProcessEvent, RemoteProcessId, RemoteProcessRequest,
    RemoteProcessStream, RemoteProcessSummary, RemoteStreamKind,
};
