use crate::{
    api_error::ApiError,
    auth::{self, ControlToken, HarnessToken},
    db::Database,
    execution::{self, ExecutionPolicy},
    harnesses, sessions,
};
use axum::{
    Json, Router,
    extract::{DefaultBodyLimit, State},
    http::StatusCode,
    middleware,
    response::{IntoResponse as _, Response},
    routing::get,
};
use serde::Serialize;

#[derive(Clone)]
pub struct AppState {
    database: Database,
    broker_client: Option<async_nats::Client>,
    control_token: ControlToken,
    harness_token: HarnessToken,
    execution_policy: ExecutionPolicy,
}
impl AppState {
    #[must_use]
    pub const fn new(
        database: Database,
        control_token: ControlToken,
        harness_token: HarnessToken,
        execution_policy: ExecutionPolicy,
    ) -> Self {
        Self {
            database,
            broker_client: None,
            control_token,
            harness_token,
            execution_policy,
        }
    }
    #[must_use]
    pub const fn database(&self) -> &Database {
        &self.database
    }
    #[must_use]
    pub const fn control_token(&self) -> &ControlToken {
        &self.control_token
    }
    #[must_use]
    pub const fn harness_token(&self) -> &HarnessToken {
        &self.harness_token
    }
    #[must_use]
    pub const fn execution_policy(&self) -> ExecutionPolicy {
        self.execution_policy
    }
    #[must_use]
    pub fn with_broker_client(mut self, client: async_nats::Client) -> Self {
        self.broker_client = Some(client);
        self
    }
}
pub fn router(state: AppState, max_request_bytes: usize) -> Router {
    let control = harnesses::router()
        .merge(sessions::router())
        .merge(execution::router())
        .route_layer(middleware::from_fn_with_state(
            state.control_token().clone(),
            auth::require_control,
        ));
    let harness = execution::harness_router().route_layer(middleware::from_fn_with_state(
        state.harness_token().clone(),
        auth::require_harness,
    ));
    Router::new()
        .route("/health", get(health))
        .route("/ready", get(readiness))
        .merge(control)
        .merge(harness)
        .fallback(not_found)
        .method_not_allowed_fallback(method_not_allowed)
        .with_state(state)
        .layer(DefaultBodyLimit::max(max_request_bytes))
}
async fn health() -> Json<HealthResponse> {
    Json(HealthResponse { status: "ok" })
}
async fn readiness(State(state): State<AppState>) -> Response {
    match state.database.health_check().await {
        Ok(())
            if state.broker_client.as_ref().is_none_or(|client| {
                client.connection_state() == async_nats::connection::State::Connected
            }) =>
        {
            Json(HealthResponse { status: "ready" }).into_response()
        }
        Ok(()) => (
            StatusCode::SERVICE_UNAVAILABLE,
            Json(HealthResponse {
                status: "unavailable",
            }),
        )
            .into_response(),
        Err(error) => {
            tracing::warn!(%error, "agent database readiness check failed");
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
async fn not_found() -> ApiError {
    ApiError::not_found()
}
async fn method_not_allowed() -> ApiError {
    ApiError::method_not_allowed()
}
#[derive(Serialize)]
struct HealthResponse {
    status: &'static str,
}
