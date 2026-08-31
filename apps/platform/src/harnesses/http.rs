use axum::{
    Json, Router,
    extract::{Path, State},
    http::{HeaderValue, header::CACHE_CONTROL},
    middleware,
    response::Response,
    routing::get,
};

use super::{
    HarnessService,
    model::{HarnessModelOptions, HarnessSummary},
};
use crate::{AppState, error::ApiError};

#[derive(Clone)]
struct HarnessState {
    service: HarnessService,
}

pub(super) fn router(service: HarnessService) -> Router<AppState> {
    Router::new()
        .route("/api/harnesses", get(list_harnesses))
        .route(
            "/api/harnesses/{harness_id}/model-options",
            get(get_model_options),
        )
        .layer(middleware::map_response(add_no_store))
        .with_state(HarnessState { service })
}

async fn get_model_options(
    State(state): State<HarnessState>,
    Path(harness_id): Path<String>,
) -> Result<Json<HarnessModelOptions>, ApiError> {
    Ok(Json(state.service.model_options(&harness_id).await?))
}

async fn list_harnesses(
    State(state): State<HarnessState>,
) -> Result<Json<Vec<HarnessSummary>>, ApiError> {
    Ok(Json(state.service.list().await?))
}

async fn add_no_store(mut response: Response) -> Response {
    response
        .headers_mut()
        .insert(CACHE_CONTROL, HeaderValue::from_static("no-store"));
    response
}
