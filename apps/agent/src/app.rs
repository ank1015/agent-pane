use axum::{
    Json, Router,
    extract::{DefaultBodyLimit, State},
    http::StatusCode,
    middleware,
    response::{IntoResponse as _, Response},
    routing::get,
};
use serde::Serialize;

use crate::{
    api_error::ApiError,
    auth::{self, ControlToken, WorkerToken},
    db::Database,
    execution::{self, ExecutionPolicy},
    harnesses, sessions,
};

#[derive(Clone)]
pub struct AppState {
    database: Database,
    control_token: ControlToken,
    worker_token: WorkerToken,
    execution_policy: ExecutionPolicy,
}

impl AppState {
    #[must_use]
    pub const fn new(
        database: Database,
        control_token: ControlToken,
        worker_token: WorkerToken,
        execution_policy: ExecutionPolicy,
    ) -> Self {
        Self {
            database,
            control_token,
            worker_token,
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
    pub const fn worker_token(&self) -> &WorkerToken {
        &self.worker_token
    }

    #[must_use]
    pub const fn execution_policy(&self) -> ExecutionPolicy {
        self.execution_policy
    }
}

pub fn router(state: AppState, max_request_bytes: usize) -> Router {
    let control_routes = harnesses::router()
        .merge(sessions::router())
        .merge(execution::router())
        .route_layer(middleware::from_fn_with_state(
            state.control_token().clone(),
            auth::require_control,
        ));
    let worker_routes = execution::worker_router().route_layer(middleware::from_fn_with_state(
        state.worker_token().clone(),
        auth::require_worker,
    ));

    Router::new()
        .route("/health", get(health))
        .route("/ready", get(readiness))
        .merge(control_routes)
        .merge(worker_routes)
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
        Ok(()) => Json(HealthResponse { status: "ready" }).into_response(),
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
