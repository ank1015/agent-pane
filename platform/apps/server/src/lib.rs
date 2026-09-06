pub mod config;
mod error;
pub mod machines;
pub mod projects;
pub mod providers;
pub mod runtime;

use axum::Router;
use machines::{ExecutionGatewayClient, MachineService};
use providers::{LlmGatewayClient, ProviderService};

pub fn router(execution_gateway: ExecutionGatewayClient, llm_gateway: LlmGatewayClient) -> Router {
    Router::new()
        .merge(machines::router(MachineService::new(execution_gateway)))
        .merge(providers::router(ProviderService::new(llm_gateway)))
}

pub mod sites;
