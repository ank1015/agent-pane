use axum::{
    Json, Router,
    extract::{Path, Query, State},
    http::{HeaderMap, StatusCode},
    routing::{get, post},
};
use execution_contracts::{EnvironmentId, PathSpec, Validate};
use execution_protocol::{
    CreateEnvironmentRequest, CreateOperationRequest, Environment, OperationRecord,
};
use serde::Deserialize;
use uuid::Uuid;

use crate::{
    db::DbError,
    http::{ApiError, AppState, require},
};

pub(crate) fn routes() -> Router<AppState> {
    Router::new()
        .route(
            "/v1/control/environments",
            get(list_environments).post(create_environment),
        )
        .route(
            "/v1/control/environments/{environment_id}",
            get(get_control_environment)
                .patch(update_environment_name)
                .delete(delete_environment),
        )
        .route("/v1/environments/{environment_id}", get(get_environment))
        .route(
            "/v1/environments/{environment_id}/operations",
            post(create_environment_operation),
        )
}

async fn create_environment(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(request): Json<CreateEnvironmentRequest>,
) -> Result<(StatusCode, Json<Environment>), ApiError> {
    require(&headers, &state.control_token)?;
    let name = request.name.trim();
    if name.is_empty() {
        return Err(ApiError::bad("environment name must not be empty"));
    }
    let machine = state
        .database
        .machine(request.machine_id.as_str())
        .await
        .map_err(ApiError::database)?
        .ok_or_else(ApiError::not_found)?;
    if !machine
        .summary
        .descriptor
        .workspace_roots
        .iter()
        .any(|root| root.id == request.workspace_root_id)
    {
        return Err(ApiError::bad(format!(
            "workspace root {:?} is not exposed by machine {:?}",
            request.workspace_root_id.as_str(),
            request.machine_id.as_str()
        )));
    }
    let path = normalize_path(&request)?;
    let environment_id =
        EnvironmentId::new(Uuid::now_v7().to_string()).expect("UUID environment ID is valid");
    let environment = match state
        .database
        .create_environment(
            &environment_id,
            &request.machine_id,
            name,
            &request.workspace_root_id,
            &path,
        )
        .await
    {
        Ok(Some(environment)) => environment,
        Ok(None) => return Err(ApiError::not_found()),
        Err(error) if is_unique_violation(&error) => {
            return Err(ApiError::conflict(
                "an active environment already exists at this machine location",
            ));
        }
        Err(error) => return Err(ApiError::database(error)),
    };
    Ok((StatusCode::CREATED, Json(environment)))
}

#[derive(Deserialize)]
struct EnvironmentQuery {
    machine_id: Option<String>,
}

async fn list_environments(
    State(state): State<AppState>,
    headers: HeaderMap,
    Query(query): Query<EnvironmentQuery>,
) -> Result<Json<Vec<Environment>>, ApiError> {
    require(&headers, &state.control_token)?;
    state
        .database
        .environments(query.machine_id.as_deref())
        .await
        .map(Json)
        .map_err(ApiError::database)
}

async fn get_control_environment(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(environment_id): Path<String>,
) -> Result<Json<Environment>, ApiError> {
    require(&headers, &state.control_token)?;
    find_environment(&state, &environment_id).await.map(Json)
}

async fn get_environment(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(environment_id): Path<String>,
) -> Result<Json<Environment>, ApiError> {
    require(&headers, &state.api_token)?;
    find_environment(&state, &environment_id).await.map(Json)
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct UpdateEnvironmentNameRequest {
    name: String,
}

async fn update_environment_name(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(environment_id): Path<String>,
    Json(request): Json<UpdateEnvironmentNameRequest>,
) -> Result<Json<Environment>, ApiError> {
    require(&headers, &state.control_token)?;
    let name = request.name.trim();
    if name.is_empty() {
        return Err(ApiError::bad("environment name must not be empty"));
    }
    state
        .database
        .update_environment_name(&environment_id, name)
        .await
        .map_err(ApiError::database)?
        .map(Json)
        .ok_or_else(ApiError::not_found)
}

async fn delete_environment(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(environment_id): Path<String>,
) -> Result<StatusCode, ApiError> {
    require(&headers, &state.control_token)?;
    state
        .sandbox_materializer
        .delete_environment(&environment_id)
        .await
        .map_err(ApiError::sandbox_materialization)?
        .then_some(StatusCode::NO_CONTENT)
        .ok_or_else(ApiError::not_found)
}

async fn create_environment_operation(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(environment_id): Path<String>,
    Json(request): Json<CreateOperationRequest>,
) -> Result<(StatusCode, Json<OperationRecord>), ApiError> {
    require(&headers, &state.api_token)?;
    let environment = find_environment(&state, &environment_id).await?;
    let machine = state
        .database
        .machine(environment.machine_id.as_str())
        .await
        .map_err(ApiError::database)?
        .ok_or_else(ApiError::not_found)?;
    let record = state
        .database
        .create_operation(
            environment.machine_id.as_str(),
            Some(&environment.environment_id),
            &request.operation,
        )
        .await
        .map_err(ApiError::database)?;
    let operation_id =
        Uuid::parse_str(&record.operation_id).map_err(|error| ApiError::bad(error.to_string()))?;
    state.router.spawn(operation_id, machine, request.operation);
    Ok((StatusCode::ACCEPTED, Json(record)))
}

async fn find_environment(state: &AppState, environment_id: &str) -> Result<Environment, ApiError> {
    state
        .database
        .environment(environment_id)
        .await
        .map_err(ApiError::database)?
        .ok_or_else(ApiError::not_found)
}

fn normalize_path(request: &CreateEnvironmentRequest) -> Result<String, ApiError> {
    PathSpec::workspace(request.workspace_root_id.clone(), request.path.clone())
        .validate()
        .map_err(|error| ApiError::bad(error.to_string()))?;
    let parts = request
        .path
        .split('/')
        .filter(|part| !part.is_empty() && *part != ".")
        .collect::<Vec<_>>();
    Ok(if parts.is_empty() {
        ".".to_owned()
    } else {
        parts.join("/")
    })
}

fn is_unique_violation(error: &DbError) -> bool {
    matches!(error, DbError::Sql(sqlx::Error::Database(error)) if error.is_unique_violation())
}
