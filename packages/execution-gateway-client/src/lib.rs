//! HTTP-backed implementation of [`execution_runtime::ExecutionEnvironment`].
//!
//! The client submits typed [`execution_protocol::Operation`] values to an
//! execution gateway and exposes a selected machine through the transport-neutral
//! execution runtime traits.

mod artifacts;
mod client;
mod config;
mod environment;
mod error;
mod filesystem;
mod operations;
mod process;
mod workspace;

pub use client::ExecutionGatewayClient;
pub use config::ExecutionGatewayConfig;
pub use environment::GatewayExecutionEnvironment;
pub use error::ExecutionGatewayClientError;
