//! Agent Pane dashboard backend.

pub mod config;
pub mod error;
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
use upstream::llm_gateway::LlmGatewayClient;

#[derive(Clone)]
struct AppState {
    gateway: LlmGatewayClient,
}

pub fn router(
    gateway: LlmGatewayClient,
    chatgpt_login: ChatGptLoginService,
    max_request_bytes: usize,
) -> Router {
    let state = AppState {
        gateway: gateway.clone(),
    };

    Router::new()
        .route("/health", get(health))
        .route("/ready", get(readiness))
        .merge(providers::router(
            ProviderService::new(gateway),
            chatgpt_login,
        ))
        .layer(DefaultBodyLimit::max(max_request_bytes))
        .with_state(state)
}

async fn health() -> Json<HealthResponse> {
    Json(HealthResponse { status: "ok" })
}

async fn readiness(axum::extract::State(state): axum::extract::State<AppState>) -> Response {
    match state.gateway.ready().await {
        Ok(()) => Json(HealthResponse { status: "ready" }).into_response(),
        Err(error) => {
            tracing::warn!(%error, "llm gateway readiness check failed");
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
