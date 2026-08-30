//! Reusable server infrastructure for independent Agent harness services.

mod active_turn;
mod agent;
mod command_bus;
mod config;
mod registry;
mod runtime;
mod server;

pub use active_turn::{ActiveTurn, ActiveTurnError};
pub use agent::{AgentClient, AgentClientError};
pub use config::{
    AgentControlServiceConfig, AgentServiceConfig, BrokerConfig, HarnessServerConfig,
};
pub use registry::{
    CreateHarnessRequest, HarnessProvider, HarnessRegistryClient, HarnessRegistryError,
    RegisterHarnessRevisionRequest, UpdateHarnessRequest,
};
pub use runtime::{HarnessRuntime, TurnOutcome};
pub use server::{HarnessDescriptor, HarnessServer, HarnessServerError};
