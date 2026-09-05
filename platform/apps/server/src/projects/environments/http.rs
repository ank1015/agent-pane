use super::{
    error::EnvironmentError,
    model::{CreateEnvironment, UpdateEnvironment},
    service::EnvironmentService,
};
use axum::{
    Json, Router,
    extract::{
        DefaultBodyLimit, Path, State,
        rejection::{JsonRejection, PathRejection},
    },
    http::{HeaderValue, StatusCode, header::CACHE_CONTROL},
    middleware,
    response::{IntoResponse, Response},
    routing::get,
};
use uuid::Uuid;

pub fn router(service: EnvironmentService) -> Router {
    Router::new()
        .route(
            "/api/projects/{project_id}/environments",
            get(list).post(create),
        )
        .route(
            "/api/projects/{project_id}/environments/{id}",
            get(detail).patch(update).delete(delete),
        )
        .layer(DefaultBodyLimit::max(16 * 1024))
        .layer(middleware::map_response(no_store))
        .with_state(service)
}

fn path_error(_: PathRejection) -> EnvironmentError {
    EnvironmentError::Invalid("Project and environment IDs must be valid UUIDs.")
}
fn json_error(error: JsonRejection) -> EnvironmentError {
    EnvironmentError::Json(error.status())
}

async fn list(
    State(service): State<EnvironmentService>,
    path: Result<Path<Uuid>, PathRejection>,
) -> Result<Response, EnvironmentError> {
    Ok(Json(service.list(path.map_err(path_error)?.0).await?).into_response())
}
async fn create(
    State(service): State<EnvironmentService>,
    path: Result<Path<Uuid>, PathRejection>,
    input: Result<Json<CreateEnvironment>, JsonRejection>,
) -> Result<Response, EnvironmentError> {
    Ok((
        StatusCode::CREATED,
        Json(
            service
                .create(path.map_err(path_error)?.0, input.map_err(json_error)?.0)
                .await?,
        ),
    )
        .into_response())
}
async fn detail(
    State(service): State<EnvironmentService>,
    path: Result<Path<(Uuid, Uuid)>, PathRejection>,
) -> Result<Response, EnvironmentError> {
    let Path((project, id)) = path.map_err(path_error)?;
    Ok(Json(service.get(project, id).await?).into_response())
}
async fn update(
    State(service): State<EnvironmentService>,
    path: Result<Path<(Uuid, Uuid)>, PathRejection>,
    input: Result<Json<UpdateEnvironment>, JsonRejection>,
) -> Result<Response, EnvironmentError> {
    let Path((project, id)) = path.map_err(path_error)?;
    Ok(Json(
        service
            .update(project, id, input.map_err(json_error)?.0)
            .await?,
    )
    .into_response())
}
async fn delete(
    State(service): State<EnvironmentService>,
    path: Result<Path<(Uuid, Uuid)>, PathRejection>,
) -> Result<StatusCode, EnvironmentError> {
    let Path((project, id)) = path.map_err(path_error)?;
    service.delete(project, id).await?;
    Ok(StatusCode::NO_CONTENT)
}
async fn no_store(mut response: Response) -> Response {
    response
        .headers_mut()
        .insert(CACHE_CONTROL, HeaderValue::from_static("no-store"));
    response
}
