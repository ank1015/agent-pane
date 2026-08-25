//! Reference execution backend for the current device.
//!
//! The same implementation is intended for personal computers, VMs, EC2
//! instances, and small Linux devices. Platform differences are isolated behind
//! internal modules rather than exposed as different machine providers.

mod artifacts;
mod error;
mod filesystem;
mod mutation;
mod path_resolver;
mod platform;
mod process;
mod query;
mod runtime;

pub use error::LocalExecutionError;
pub use path_resolver::LocalNativeGrant;
pub use runtime::{LocalExecutionRuntime, LocalRuntimeConfig, LocalWorkspaceRoot};
