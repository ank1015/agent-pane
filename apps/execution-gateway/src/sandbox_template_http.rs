use axum::{
    Json, Router,
    extract::{Path, Query, State},
    http::{HeaderMap, StatusCode},
    routing::get,
};
use execution_contracts::WorkspaceRootId;
use execution_protocol::{
    CreateSandboxTemplateEnvironmentRequest as CreateApiSandboxTemplateEnvironmentRequest,
    ProjectEnvironment,
    UpdateSandboxTemplateEnvironmentRequest as UpdateApiSandboxTemplateEnvironmentRequest,
};
use serde::Deserialize;
use uuid::Uuid;

use crate::{
    db::DbError,
    http::{ApiError, AppState, require},
    sandbox_runtime::sandbox_workspace_root,
    sandbox_templates::{
        CreateSandboxEnvironmentTemplateRequest, SandboxEnvironmentInstance,
        SandboxEnvironmentTemplate, UpdateSandboxEnvironmentTemplateRequest, environment_path,
        validate_create, validate_update,
    },
};

pub(crate) fn routes() -> Router<AppState> {
    Router::new()
        .route(
            "/v1/sandbox-environment-templates",
            axum::routing::post(create_api_template),
        )
        .route(
            "/v1/sandbox-environment-templates/{template_id}",
            axum::routing::patch(update_api_template),
        )
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

async fn create_api_template(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(request): Json<CreateApiSandboxTemplateEnvironmentRequest>,
) -> Result<(StatusCode, Json<ProjectEnvironment>), ApiError> {
    require(&headers, &state.api_token)?;
    let project_id = request.project_id;
    let cwd = api_template_cwd(&state, request.snapshot_id, &request.path).await?;
    let template = create_template_record(
        &state,
        CreateSandboxEnvironmentTemplateRequest {
            project_id,
            name: request.name,
            snapshot_id: request.snapshot_id,
            cwd,
            creation_script: request.creation_script,
        },
    )
    .await?;
    project_environment(&state, project_id, &template.id.to_string())
        .await
        .map(|environment| (StatusCode::CREATED, Json(environment)))
}

async fn update_api_template(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(template_id): Path<Uuid>,
    Json(request): Json<UpdateApiSandboxTemplateEnvironmentRequest>,
) -> Result<Json<ProjectEnvironment>, ApiError> {
    require(&headers, &state.api_token)?;
    if request.name.is_none()
        && request.snapshot_id.is_none()
        && request.path.is_none()
        && request.creation_script.is_none()
    {
        return Err(ApiError::bad("at least one field must be supplied"));
    }
    let current = find_template(&state, template_id).await?;
    if current.project_id != request.project_id {
        return Err(ApiError::not_found());
    }
    let snapshot_id = request.snapshot_id.unwrap_or(current.snapshot_id);
    let cwd = match request.path.as_deref() {
        Some(path) => Some(api_template_cwd(&state, snapshot_id, path).await?),
        None if request.snapshot_id.is_some() => {
            let relative = environment_path(&current.cwd, sandbox_workspace_root(current.provider))
                .map_err(ApiError::bad)?;
            Some(api_template_cwd(&state, snapshot_id, &relative).await?)
        }
        None => None,
    };
    let mut update = UpdateSandboxEnvironmentTemplateRequest {
        name: request.name,
        snapshot_id: request.snapshot_id,
        cwd,
        creation_script: request.creation_script,
    };
    validate_update(&mut update).map_err(ApiError::bad)?;
    validate_template_location(
        &state,
        update.snapshot_id.unwrap_or(current.snapshot_id),
        update.cwd.as_deref().unwrap_or(&current.cwd),
    )
    .await?;
    match state
        .database
        .update_sandbox_environment_template(template_id, &update)
        .await
    {
        Ok(Some(_)) => project_environment(&state, request.project_id, &template_id.to_string())
            .await
            .map(Json),
        Ok(None) => Err(ApiError::not_found()),
        Err(error) if is_unique_violation(&error) => Err(ApiError::conflict(
            "an active sandbox environment template with this name already exists",
        )),
        Err(error) => Err(ApiError::database(error)),
    }
}

async fn create_template(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(request): Json<CreateSandboxEnvironmentTemplateRequest>,
) -> Result<(StatusCode, Json<SandboxEnvironmentTemplate>), ApiError> {
    require(&headers, &state.control_token)?;
    create_template_record(&state, request)
        .await
        .map(|template| (StatusCode::CREATED, Json(template)))
}

async fn create_template_record(
    state: &AppState,
    mut request: CreateSandboxEnvironmentTemplateRequest,
) -> Result<SandboxEnvironmentTemplate, ApiError> {
    validate_create(&mut request).map_err(ApiError::bad)?;
    validate_template_location(state, request.snapshot_id, &request.cwd).await?;
    match state
        .database
        .create_sandbox_environment_template(Uuid::now_v7(), &request)
        .await
    {
        Ok(Some(template)) => Ok(template),
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
    Query(query): Query<TemplateQuery>,
) -> Result<Json<Vec<SandboxEnvironmentTemplate>>, ApiError> {
    require(&headers, &state.control_token)?;
    state
        .database
        .sandbox_environment_templates(query.project_id)
        .await
        .map(Json)
        .map_err(ApiError::database)
}

#[derive(Deserialize)]
struct TemplateQuery {
    project_id: Option<Uuid>,
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
    let current = find_template(&state, template_id).await?;
    validate_template_location(
        &state,
        request.snapshot_id.unwrap_or(current.snapshot_id),
        request.cwd.as_deref().unwrap_or(&current.cwd),
    )
    .await?;
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
    let materializer = state.sandbox_materializer.clone();
    // Remote sandbox provisioning may outlive the HTTP caller. Running the
    // workflow in an owned task lets it finish its normal success or cleanup
    // path even if the client disconnects while waiting for the response.
    tokio::spawn(async move { materializer.materialize(template_id).await })
        .await
        .map_err(ApiError::background_task)?
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

async fn validate_template_location(
    state: &AppState,
    snapshot_id: Uuid,
    cwd: &str,
) -> Result<(), ApiError> {
    let snapshot = state
        .database
        .snapshot(snapshot_id)
        .await
        .map_err(ApiError::database)?
        .ok_or_else(ApiError::not_found)?;
    environment_path(cwd, sandbox_workspace_root(snapshot.provider))
        .map(drop)
        .map_err(ApiError::bad)
}

async fn api_template_cwd(
    state: &AppState,
    snapshot_id: Uuid,
    path: &str,
) -> Result<String, ApiError> {
    let snapshot = state
        .database
        .snapshot(snapshot_id)
        .await
        .map_err(ApiError::database)?
        .ok_or_else(ApiError::not_found)?;
    let root_id = WorkspaceRootId::new("sandbox-workspace")
        .expect("static sandbox workspace root ID is valid");
    let relative = crate::environments::normalize_workspace_path(path, &root_id)?;
    let root = sandbox_workspace_root(snapshot.provider);
    Ok(if relative == "." {
        root.to_owned()
    } else {
        format!("{root}/{relative}")
    })
}

async fn project_environment(
    state: &AppState,
    project_id: Uuid,
    environment_id: &str,
) -> Result<ProjectEnvironment, ApiError> {
    state
        .database
        .project_environments(project_id)
        .await
        .map_err(ApiError::database)?
        .into_iter()
        .find(|environment| environment.id == environment_id)
        .ok_or_else(ApiError::not_found)
}

fn is_unique_violation(error: &DbError) -> bool {
    matches!(error, DbError::Sql(sqlx::Error::Database(error)) if error.is_unique_violation())
}
