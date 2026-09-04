use axum::{
    Json, Router,
    extract::{
        DefaultBodyLimit, Path, Query, State,
        rejection::{JsonRejection, PathRejection, QueryRejection},
    },
    http::{HeaderValue, StatusCode, header::CACHE_CONTROL},
    middleware,
    response::{IntoResponse, Response},
    routing::get,
};

use super::analytics::{ProviderRequestPage, ProviderUsage, RequestsQuery};
use super::model::CreateProviderInput;
use super::{
    ProviderAccountSummary, ProviderDetailResponse, ProviderService, error::ProviderError,
};
use uuid::Uuid;

pub fn router(service: ProviderService) -> Router {
    Router::new()
        .route("/api/providers", get(list_accounts).post(create_account))
        .route("/api/providers/{provider_id}", get(get_account))
        .route("/api/providers/{provider_id}/usage", get(get_usage))
        .route("/api/providers/{provider_id}/requests", get(get_requests))
        .layer(DefaultBodyLimit::max(16 * 1024))
        .layer(middleware::map_response(no_store))
        .with_state(service)
}

async fn get_account(
    State(service): State<ProviderService>,
    path: Result<Path<Uuid>, PathRejection>,
) -> Result<Json<ProviderDetailResponse>, ProviderError> {
    let Path(provider_id) = path.map_err(|_| ProviderError::InvalidProviderId)?;
    Ok(Json(ProviderDetailResponse {
        provider: service.get_account(provider_id).await?,
    }))
}

async fn get_usage(
    State(service): State<ProviderService>,
    path: Result<Path<Uuid>, PathRejection>,
) -> Result<Json<ProviderUsage>, ProviderError> {
    let Path(id) = path.map_err(|_| ProviderError::InvalidProviderId)?;
    Ok(Json(service.usage(id).await?))
}

async fn get_requests(
    State(service): State<ProviderService>,
    path: Result<Path<Uuid>, PathRejection>,
    query: Result<Query<RequestsQuery>, QueryRejection>,
) -> Result<Json<ProviderRequestPage>, ProviderError> {
    let Path(id) = path.map_err(|_| ProviderError::InvalidProviderId)?;
    let Query(query) =
        query.map_err(|_| ProviderError::InvalidRequest("Invalid request-history query."))?;
    Ok(Json(service.requests(id, query).await?))
}

async fn list_accounts(
    State(service): State<ProviderService>,
) -> Result<Json<Vec<ProviderAccountSummary>>, ProviderError> {
    Ok(Json(service.list_accounts().await?))
}

pub(super) async fn no_store(mut response: Response) -> Response {
    response
        .headers_mut()
        .insert(CACHE_CONTROL, HeaderValue::from_static("no-store"));
    response
}

async fn create_account(
    State(service): State<ProviderService>,
    payload: Result<Json<CreateProviderInput>, JsonRejection>,
) -> Result<Response, ProviderError> {
    let Json(input) = payload.map_err(|error| ProviderError::InvalidJson(error.status()))?;
    Ok((
        StatusCode::CREATED,
        Json(service.create_account(input).await?),
    )
        .into_response())
}
