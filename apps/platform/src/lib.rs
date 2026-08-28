//! Agent Pane dashboard backend.

pub mod config;
pub mod error;
pub mod harnesses;
pub mod machines;
pub mod providers;
pub mod upstream;

use axum::{
    Json, Router,
    extract::DefaultBodyLimit,
    http::StatusCode,
    response::{IntoResponse, Response},
    routing::get,
};
use serde::Serialize;

use providers::{ProviderService, chatgpt_oauth::ChatGptLoginService};
use upstream::{
    agent::AgentClient, execution_gateway::ExecutionGatewayClient, llm_gateway::LlmGatewayClient,
};

#[derive(Clone)]
struct AppState {
    llm_gateway: LlmGatewayClient,
    execution_gateway: ExecutionGatewayClient,
    agent: AgentClient,
}

pub fn router(
    gateway: LlmGatewayClient,
    execution_gateway: ExecutionGatewayClient,
    agent: AgentClient,
    chatgpt_login: ChatGptLoginService,
    max_request_bytes: usize,
) -> Router {
    let state = AppState {
        llm_gateway: gateway.clone(),
        execution_gateway: execution_gateway.clone(),
        agent: agent.clone(),
    };

    Router::new()
        .route("/health", get(health))
        .route("/ready", get(readiness))
        .merge(providers::router(
            ProviderService::new(gateway),
            chatgpt_login,
        ))
        .merge(machines::router(machines::MachineService::new(
            execution_gateway,
        )))
        .merge(harnesses::router(harnesses::HarnessService::new(agent)))
        .layer(DefaultBodyLimit::max(max_request_bytes))
        .with_state(state)
}

async fn health() -> Json<HealthResponse> {
    Json(HealthResponse { status: "ok" })
}

async fn readiness(axum::extract::State(state): axum::extract::State<AppState>) -> Response {
    let (llm_gateway, execution_gateway, agent) = tokio::join!(
        state.llm_gateway.ready(),
        state.execution_gateway.ready(),
        state.agent.ready()
    );
    match (llm_gateway, execution_gateway, agent) {
        (Ok(()), Ok(()), Ok(())) => Json(HealthResponse { status: "ready" }).into_response(),
        (llm_gateway, execution_gateway, agent) => {
            if let Err(error) = llm_gateway {
                tracing::warn!(%error, "llm gateway readiness check failed");
            }
            if let Err(error) = execution_gateway {
                tracing::warn!(%error, "execution gateway readiness check failed");
            }
            if let Err(error) = agent {
                tracing::warn!(%error, "Agent readiness check failed");
            }
            (
                StatusCode::SERVICE_UNAVAILABLE,
                Json(HealthResponse {
                    status: "unavailable",
                }),
            )
                .into_response()
        }
    }
}

#[derive(Serialize)]
struct HealthResponse {
    status: &'static str,
}
