use axum::{
    Json, Router,
    extract::{Path, State, rejection::JsonRejection},
    http::{HeaderValue, StatusCode, header::CACHE_CONTROL},
    middleware,
    response::{IntoResponse, Response},
    routing::get,
};
use execution_protocol::ProjectEnvironment;
use uuid::Uuid;

use super::{
    ProjectService,
    model::{CreateProjectRequest, Project, UpdateProjectRequest},
};
use crate::{AppState, error::ApiError};

#[derive(Clone)]
struct ProjectState {
    service: ProjectService,
}

pub(super) fn router(service: ProjectService) -> Router<AppState> {
    Router::new()
        .route("/api/projects", get(list_projects).post(create_project))
        .route(
            "/api/projects/{project_id}",
            get(get_project)
                .patch(update_project)
                .delete(delete_project),
        )
        .route(
            "/api/projects/{project_id}/environments",
            get(list_project_environments),
        )
        .layer(middleware::map_response(add_no_store))
        .with_state(ProjectState { service })
}

async fn list_projects(State(state): State<ProjectState>) -> Result<Json<Vec<Project>>, ApiError> {
    Ok(Json(state.service.list().await?))
}

async fn get_project(
    State(state): State<ProjectState>,
    Path(project_id): Path<Uuid>,
) -> Result<Json<Project>, ApiError> {
    Ok(Json(state.service.get(project_id).await?))
}

async fn list_project_environments(
    State(state): State<ProjectState>,
    Path(project_id): Path<Uuid>,
) -> Result<Json<Vec<ProjectEnvironment>>, ApiError> {
    Ok(Json(state.service.list_environments(project_id).await?))
}

async fn create_project(
    State(state): State<ProjectState>,
    payload: Result<Json<CreateProjectRequest>, JsonRejection>,
) -> Result<Response, ApiError> {
    let Json(request) = payload.map_err(|error| ApiError::invalid_request(error.body_text()))?;
    let project = state.service.create(&request).await?;
    Ok((StatusCode::CREATED, Json(project)).into_response())
}

async fn update_project(
    State(state): State<ProjectState>,
    Path(project_id): Path<Uuid>,
    payload: Result<Json<UpdateProjectRequest>, JsonRejection>,
) -> Result<Json<Project>, ApiError> {
    let Json(request) = payload.map_err(|error| ApiError::invalid_request(error.body_text()))?;
    Ok(Json(state.service.update(project_id, &request).await?))
}

async fn delete_project(
    State(state): State<ProjectState>,
    Path(project_id): Path<Uuid>,
) -> Result<StatusCode, ApiError> {
    state.service.delete(project_id).await?;
    Ok(StatusCode::NO_CONTENT)
}

async fn add_no_store(mut response: Response) -> Response {
    response
        .headers_mut()
        .insert(CACHE_CONTROL, HeaderValue::from_static("no-store"));
    response
}
