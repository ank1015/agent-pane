//! HTTP-backed implementation of [`execution_runtime::ExecutionRuntime`].
//!
//! The client submits typed [`execution_protocol::Operation`] values to an
//! execution gateway and exposes either a selected machine or a saved environment
//! through the transport-neutral execution runtime traits.

mod artifacts;
mod client;
mod config;
mod environment;
mod error;
mod filesystem;
mod machine;
mod operations;
mod process;
mod workspace;

pub use client::ExecutionGatewayClient;
pub use config::ExecutionGatewayConfig;
pub use environment::GatewayEnvironment;
pub use error::ExecutionGatewayClientError;
pub use machine::GatewayMachineRuntime;
