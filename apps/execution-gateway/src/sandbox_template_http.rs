use axum::{
    Json, Router,
    extract::{Path, State},
    http::{HeaderMap, StatusCode},
    routing::get,
};
use uuid::Uuid;

use crate::{
    db::DbError,
    http::{ApiError, AppState, require},
    sandbox_templates::{
        CreateSandboxEnvironmentTemplateRequest, SandboxEnvironmentInstance,
        SandboxEnvironmentTemplate, UpdateSandboxEnvironmentTemplateRequest, validate_create,
        validate_update,
    },
};

pub(crate) fn routes() -> Router<AppState> {
    Router::new()
        .route(
            "/v1/control/sandbox-environment-templates",
            get(list_templates).post(create_template),
        )
        .route(
            "/v1/control/sandbox-environment-templates/{template_id}",
            get(get_template)
                .patch(update_template)
                .delete(delete_template),
        )
        .route(
            "/v1/control/sandbox-environment-templates/{template_id}/environments",
            get(list_instances).post(materialize_template),
        )
}

async fn create_template(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(mut request): Json<CreateSandboxEnvironmentTemplateRequest>,
) -> Result<(StatusCode, Json<SandboxEnvironmentTemplate>), ApiError> {
    require(&headers, &state.control_token)?;
    validate_create(&mut request).map_err(ApiError::bad)?;
    match state
        .database
        .create_sandbox_environment_template(Uuid::now_v7(), &request)
        .await
    {
        Ok(Some(template)) => Ok((StatusCode::CREATED, Json(template))),
        Ok(None) => Err(ApiError::not_found()),
        Err(error) if is_unique_violation(&error) => Err(ApiError::conflict(
            "an active sandbox environment template with this name already exists",
        )),
        Err(error) => Err(ApiError::database(error)),
    }
}

async fn list_templates(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> Result<Json<Vec<SandboxEnvironmentTemplate>>, ApiError> {
    require(&headers, &state.control_token)?;
    state
        .database
        .sandbox_environment_templates()
        .await
        .map(Json)
        .map_err(ApiError::database)
}

async fn get_template(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(template_id): Path<Uuid>,
) -> Result<Json<SandboxEnvironmentTemplate>, ApiError> {
    require(&headers, &state.control_token)?;
    find_template(&state, template_id).await.map(Json)
}

async fn update_template(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(template_id): Path<Uuid>,
    Json(mut request): Json<UpdateSandboxEnvironmentTemplateRequest>,
) -> Result<Json<SandboxEnvironmentTemplate>, ApiError> {
    require(&headers, &state.control_token)?;
    validate_update(&mut request).map_err(ApiError::bad)?;
    if let Some(snapshot_id) = request.snapshot_id
        && state
            .database
            .snapshot(snapshot_id)
            .await
            .map_err(ApiError::database)?
            .is_none()
    {
        return Err(ApiError::not_found());
    }
    match state
        .database
        .update_sandbox_environment_template(template_id, &request)
        .await
    {
        Ok(Some(template)) => Ok(Json(template)),
        Ok(None) => Err(ApiError::not_found()),
        Err(error) if is_unique_violation(&error) => Err(ApiError::conflict(
            "an active sandbox environment template with this name already exists",
        )),
        Err(error) => Err(ApiError::database(error)),
    }
}

async fn delete_template(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(template_id): Path<Uuid>,
) -> Result<StatusCode, ApiError> {
    require(&headers, &state.control_token)?;
    state
        .database
        .delete_sandbox_environment_template(template_id)
        .await
        .map_err(ApiError::database)?
        .then_some(StatusCode::NO_CONTENT)
        .ok_or_else(ApiError::not_found)
}

async fn materialize_template(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(template_id): Path<Uuid>,
) -> Result<(StatusCode, Json<SandboxEnvironmentInstance>), ApiError> {
    require(&headers, &state.control_token)?;
    state
        .sandbox_materializer
        .materialize(template_id)
        .await
        .map(|instance| (StatusCode::CREATED, Json(instance)))
        .map_err(ApiError::sandbox_materialization)
}

async fn list_instances(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(template_id): Path<Uuid>,
) -> Result<Json<Vec<SandboxEnvironmentInstance>>, ApiError> {
    require(&headers, &state.control_token)?;
    find_template(&state, template_id).await?;
    state
        .database
        .sandbox_environment_instances(template_id)
        .await
        .map(Json)
        .map_err(ApiError::database)
}

async fn find_template(
    state: &AppState,
    template_id: Uuid,
) -> Result<SandboxEnvironmentTemplate, ApiError> {
    state
        .database
        .sandbox_environment_template(template_id)
        .await
        .map_err(ApiError::database)?
        .ok_or_else(ApiError::not_found)
}

fn is_unique_violation(error: &DbError) -> bool {
    matches!(error, DbError::Sql(sqlx::Error::Database(error)) if error.is_unique_violation())
}
