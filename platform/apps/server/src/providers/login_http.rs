use axum::{
    Json, Router,
    extract::{
        DefaultBodyLimit, Path, Query, State,
        rejection::{JsonRejection, QueryRejection},
    },
    http::{StatusCode, header},
    middleware,
    response::{Html, IntoResponse, Response},
    routing::{get, post},
};
use uuid::Uuid;

use super::{
    error::ProviderError,
    http::no_store,
    login::{CallbackQuery, ChatGptLoginService, StartLoginInput},
};

pub fn login_router(service: Option<ChatGptLoginService>) -> Router {
    Router::new()
        .route("/api/providers/chatgpt/login", post(start))
        .route(
            "/api/providers/chatgpt/login/{id}",
            get(status).delete(cancel),
        )
        .layer(DefaultBodyLimit::max(4096))
        .layer(middleware::map_response(no_store))
        .with_state(service)
}

pub fn callback_router(service: ChatGptLoginService) -> Router {
    Router::new()
        .route("/auth/callback", get(callback))
        .layer(middleware::map_response(no_store))
        .with_state(service)
}

async fn start(
    State(service): State<Option<ChatGptLoginService>>,
    input: Result<Json<StartLoginInput>, JsonRejection>,
) -> Result<Response, ProviderError> {
    let service = service.ok_or(ProviderError::LoginUnavailable)?;
    let Json(input) = input.map_err(|error| ProviderError::InvalidJson(error.status()))?;
    Ok((StatusCode::CREATED, Json(service.start(input.name).await?)).into_response())
}

async fn status(
    State(service): State<Option<ChatGptLoginService>>,
    Path(id): Path<Uuid>,
) -> Result<Response, ProviderError> {
    Ok(Json(
        service
            .ok_or(ProviderError::LoginUnavailable)?
            .status(id)
            .await?,
    )
    .into_response())
}

async fn cancel(
    State(service): State<Option<ChatGptLoginService>>,
    Path(id): Path<Uuid>,
) -> Result<StatusCode, ProviderError> {
    service
        .ok_or(ProviderError::LoginUnavailable)?
        .cancel(id)
        .await?;
    Ok(StatusCode::NO_CONTENT)
}

async fn callback(
    State(service): State<ChatGptLoginService>,
    input: Result<Query<CallbackQuery>, QueryRejection>,
) -> Response {
    let success = match input {
        Ok(Query(query)) => service.complete(query).await.is_ok(),
        Err(_) => false,
    };
    // No query/error interpolation, scripts, external assets, or tokens in HTML.
    // The dashboard polls completion and closes the popup itself.
    let html = if success {
        "<!doctype html><html><title>ChatGPT connected</title><body><h1>ChatGPT connected</h1><p>You can close this window and return to the dashboard.</p></body></html>"
    } else {
        "<!doctype html><html><title>Sign-in not completed</title><body><h1>Sign-in not completed</h1><p>Return to the dashboard and start again.</p></body></html>"
    };
    (
        if success {
            StatusCode::OK
        } else {
            StatusCode::BAD_REQUEST
        },
        [
            (header::REFERRER_POLICY, "no-referrer"),
            (
                header::CONTENT_SECURITY_POLICY,
                "default-src 'none'; frame-ancestors 'none'; base-uri 'none'",
            ),
        ],
        Html(html),
    )
        .into_response()
}
