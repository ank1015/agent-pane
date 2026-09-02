use axum::{
    Json, Router,
    extract::{
        Path, Query, State,
        rejection::{JsonRejection, QueryRejection},
    },
    http::{HeaderValue, StatusCode, header::CACHE_CONTROL},
    middleware,
    response::{IntoResponse, Response},
    routing::{get, put},
};
use uuid::Uuid;

use super::{
    ProviderService,
    chatgpt_oauth::{ChatGptLoginService, StartLoginRequest},
    model::{
        CreateProviderRequest, ProviderAccountSummary, ProviderRequestPage, ProviderRequestsQuery,
        ProviderResponse, ProviderUsageSummary, RotateCredentialsRequest, UpdateProviderRequest,
    },
};
use crate::{AppState, error::ApiError};

#[derive(Clone)]
struct ProviderState {
    service: ProviderService,
    chatgpt_login: ChatGptLoginService,
}

pub(super) fn router(
    service: ProviderService,
    chatgpt_login: ChatGptLoginService,
) -> Router<AppState> {
    Router::new()
        .route("/api/providers", get(list_providers).post(create_provider))
        .route(
            "/api/providers/chatgpt/login",
            axum::routing::post(start_chatgpt_login),
        )
        .route(
            "/api/providers/chatgpt/login/{login_id}",
            get(chatgpt_login_status).delete(cancel_chatgpt_login),
        )
        .route(
            "/api/providers/{provider_id}",
            get(get_provider)
                .patch(update_provider)
                .delete(delete_provider),
        )
        .route(
            "/api/providers/{provider_id}/default",
            put(set_default_provider),
        )
        .route(
            "/api/providers/{provider_id}/credentials",
            put(rotate_credentials),
        )
        .route(
            "/api/providers/{provider_id}/usage",
            get(get_provider_usage),
        )
        .route(
            "/api/providers/{provider_id}/requests",
            get(list_provider_requests),
        )
        .layer(middleware::map_response(add_no_store))
        .with_state(ProviderState {
            service,
            chatgpt_login,
        })
}

async fn start_chatgpt_login(
    State(state): State<ProviderState>,
    payload: Result<Json<StartLoginRequest>, JsonRejection>,
) -> Result<Response, ApiError> {
    let Json(request) = payload.map_err(|error| ApiError::invalid_request(error.body_text()))?;
    let response = state
        .chatgpt_login
        .start(request.name)
        .await
        .map_err(|error| ApiError::invalid_request(error.user_message()))?;
    Ok((StatusCode::CREATED, Json(response)).into_response())
}

async fn chatgpt_login_status(
    State(state): State<ProviderState>,
    Path(login_id): Path<Uuid>,
) -> Result<Json<super::chatgpt_oauth::LoginStatus>, ApiError> {
    let response = state
        .chatgpt_login
        .status(login_id)
        .await
        .map_err(|error| ApiError::invalid_request(error.user_message()))?;
    Ok(Json(response))
}

async fn cancel_chatgpt_login(
    State(state): State<ProviderState>,
    Path(login_id): Path<Uuid>,
) -> Result<StatusCode, ApiError> {
    state
        .chatgpt_login
        .cancel(login_id)
        .await
        .map_err(|error| ApiError::invalid_request(error.user_message()))?;
    Ok(StatusCode::NO_CONTENT)
}

async fn list_providers(
    State(state): State<ProviderState>,
) -> Result<Json<Vec<ProviderAccountSummary>>, ApiError> {
    Ok(Json(state.service.list_accounts().await?))
}

async fn get_provider(
    State(state): State<ProviderState>,
    Path(provider_id): Path<Uuid>,
) -> Result<Json<ProviderResponse>, ApiError> {
    let provider = state.service.get(provider_id).await?;
    Ok(Json(ProviderResponse { provider }))
}

async fn get_provider_usage(
    State(state): State<ProviderState>,
    Path(provider_id): Path<Uuid>,
) -> Result<Json<ProviderUsageSummary>, ApiError> {
    Ok(Json(state.service.usage(provider_id).await?))
}

async fn list_provider_requests(
    State(state): State<ProviderState>,
    Path(provider_id): Path<Uuid>,
    query: Result<Query<ProviderRequestsQuery>, QueryRejection>,
) -> Result<Json<ProviderRequestPage>, ApiError> {
    let Query(query) = query.map_err(|error| ApiError::invalid_request(error.body_text()))?;
    Ok(Json(state.service.requests(provider_id, &query).await?))
}

async fn create_provider(
    State(state): State<ProviderState>,
    payload: Result<Json<CreateProviderRequest>, JsonRejection>,
) -> Result<Response, ApiError> {
    let Json(request) = payload.map_err(|error| ApiError::invalid_request(error.body_text()))?;
    let provider = state.service.create(&request).await?;
    Ok((StatusCode::CREATED, Json(ProviderResponse { provider })).into_response())
}

async fn update_provider(
    State(state): State<ProviderState>,
    Path(provider_id): Path<Uuid>,
    payload: Result<Json<UpdateProviderRequest>, JsonRejection>,
) -> Result<Json<ProviderResponse>, ApiError> {
    let Json(request) = payload.map_err(|error| ApiError::invalid_request(error.body_text()))?;
    let provider = state.service.update(provider_id, &request).await?;
    Ok(Json(ProviderResponse { provider }))
}

async fn set_default_provider(
    State(state): State<ProviderState>,
    Path(provider_id): Path<Uuid>,
) -> Result<Json<ProviderResponse>, ApiError> {
    let provider = state.service.set_default(provider_id).await?;
    Ok(Json(ProviderResponse { provider }))
}

async fn rotate_credentials(
    State(state): State<ProviderState>,
    Path(provider_id): Path<Uuid>,
    payload: Result<Json<RotateCredentialsRequest>, JsonRejection>,
) -> Result<Json<ProviderResponse>, ApiError> {
    let Json(request) = payload.map_err(|error| ApiError::invalid_request(error.body_text()))?;
    let provider = state
        .service
        .rotate_credentials(provider_id, &request)
        .await?;
    Ok(Json(ProviderResponse { provider }))
}

async fn delete_provider(
    State(state): State<ProviderState>,
    Path(provider_id): Path<Uuid>,
) -> Result<StatusCode, ApiError> {
    state.service.delete(provider_id).await?;
    Ok(StatusCode::NO_CONTENT)
}

async fn add_no_store(mut response: Response) -> Response {
    response
        .headers_mut()
        .insert(CACHE_CONTROL, HeaderValue::from_static("no-store"));
    response
}
