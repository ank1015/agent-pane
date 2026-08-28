use axum::{
    Json, Router,
    extract::State,
    http::{HeaderValue, header::CACHE_CONTROL},
    middleware,
    response::Response,
    routing::get,
};

use super::{HarnessService, model::HarnessSummary};
use crate::{AppState, error::ApiError};

#[derive(Clone)]
struct HarnessState {
    service: HarnessService,
}

pub(super) fn router(service: HarnessService) -> Router<AppState> {
    Router::new()
        .route("/api/harnesses", get(list_harnesses))
        .layer(middleware::map_response(add_no_store))
        .with_state(HarnessState { service })
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
