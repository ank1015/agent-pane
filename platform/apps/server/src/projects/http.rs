use axum::{
    Json, Router,
    extract::{DefaultBodyLimit, State, rejection::JsonRejection},
    http::{HeaderValue, StatusCode, header::CACHE_CONTROL},
    middleware,
    response::{IntoResponse, Response},
    routing::get,
};

use super::{CreateProjectInput, Project, ProjectError, ProjectService};

pub fn router(service: ProjectService) -> Router {
    Router::new()
        .route("/api/projects", get(list).post(create))
        .layer(DefaultBodyLimit::max(800 * 1024))
        .layer(middleware::map_response(no_store))
        .with_state(service)
}

async fn list(State(service): State<ProjectService>) -> Result<Json<Vec<Project>>, ProjectError> {
    Ok(Json(service.list().await?))
}

async fn create(
    State(service): State<ProjectService>,
    payload: Result<Json<CreateProjectInput>, JsonRejection>,
) -> Result<Response, ProjectError> {
    let Json(input) = payload.map_err(|error| ProjectError::InvalidJson(error.status()))?;
    Ok((StatusCode::CREATED, Json(service.create(input).await?)).into_response())
}

async fn no_store(mut response: Response) -> Response {
    response
        .headers_mut()
        .insert(CACHE_CONTROL, HeaderValue::from_static("no-store"));
    response
}
