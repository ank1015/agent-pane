//! Direct Daytona implementation of the execution runtime.
//!
//! The connector talks to Daytona's sandbox proxy APIs directly. Filesystem
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
    DaytonaConnectionConfig, DaytonaNativeGrant, DaytonaRuntimeConfig, DaytonaWorkspaceRoot,
};
pub use error::{DaytonaConnectorError, DaytonaTransportError};
pub use http::DaytonaHttpTransport;
pub use lifecycle::{ReadyDaytonaSandbox, create_from_snapshot, terminate, wait_until_ready};
pub use runtime::DaytonaExecutionRuntime;
pub use transport::{
    DaytonaTransport, RemoteProcessEvent, RemoteProcessId, RemoteProcessRequest,
    RemoteProcessStream, RemoteProcessSummary, RemoteStreamKind,
};
