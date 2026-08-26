use axum::{
    Json, Router,
    extract::{Path, Query, State},
    http::{HeaderMap, StatusCode},
    routing::get,
};
use serde::Deserialize;
use uuid::Uuid;

use crate::{
    db::DbError,
    http::{ApiError, AppState, require},
    sandbox_accounts::SandboxProvider,
    snapshots::{
        CreateSnapshotRequest, Snapshot, UpdateSnapshotRequest, validate_request,
        validate_update_request,
    },
};

pub(crate) fn routes() -> Router<AppState> {
    Router::new()
        .route(
            "/v1/control/snapshots",
            get(list_snapshots).post(create_snapshot),
        )
        .route(
            "/v1/control/snapshots/{snapshot_id}",
            get(get_snapshot)
                .patch(update_snapshot_name)
                .delete(delete_snapshot),
        )
}

async fn create_snapshot(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(request): Json<CreateSnapshotRequest>,
) -> Result<(StatusCode, Json<Snapshot>), ApiError> {
    require(&headers, &state.control_token)?;
    validate_request(&request).map_err(|field| {
        ApiError::bad(format!(
            "{field} must not be blank or have surrounding whitespace"
        ))
    })?;
    let account = state
        .sandbox_accounts
        .resolve(request.provider, request.sandbox_account_id)
        .await
        .map_err(ApiError::sandbox_account)?;
    match state
        .database
        .create_snapshot(Uuid::now_v7(), account.id, &request)
        .await
    {
        Ok(snapshot) => Ok((StatusCode::CREATED, Json(snapshot))),
        Err(error) if is_unique_violation(&error) => Err(ApiError::conflict(
            "this sandbox account already has a snapshot with this provider snapshot ID",
        )),
        Err(error) => Err(ApiError::database(error)),
    }
}

#[derive(Deserialize)]
struct SnapshotQuery {
    provider: Option<SandboxProvider>,
    sandbox_account_id: Option<Uuid>,
    sandbox_id: Option<String>,
}

async fn list_snapshots(
    State(state): State<AppState>,
    headers: HeaderMap,
    Query(query): Query<SnapshotQuery>,
) -> Result<Json<Vec<Snapshot>>, ApiError> {
    require(&headers, &state.control_token)?;
    state
        .database
        .snapshots(
            query.provider,
            query.sandbox_account_id,
            query.sandbox_id.as_deref(),
        )
        .await
        .map(Json)
        .map_err(ApiError::database)
}

async fn get_snapshot(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(snapshot_id): Path<Uuid>,
) -> Result<Json<Snapshot>, ApiError> {
    require(&headers, &state.control_token)?;
    state
        .database
        .snapshot(snapshot_id)
        .await
        .map_err(ApiError::database)?
        .map(Json)
        .ok_or_else(ApiError::not_found)
}

async fn update_snapshot_name(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(snapshot_id): Path<Uuid>,
    Json(request): Json<UpdateSnapshotRequest>,
) -> Result<Json<Snapshot>, ApiError> {
    require(&headers, &state.control_token)?;
    validate_update_request(&request).map_err(|field| {
        ApiError::bad(format!(
            "{field} must not be blank or have surrounding whitespace"
        ))
    })?;
    state
        .database
        .update_snapshot_name(snapshot_id, &request.name)
        .await
        .map_err(ApiError::database)?
        .map(Json)
        .ok_or_else(ApiError::not_found)
}

async fn delete_snapshot(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(snapshot_id): Path<Uuid>,
) -> Result<StatusCode, ApiError> {
    require(&headers, &state.control_token)?;
    state
        .database
        .delete_snapshot(snapshot_id)
        .await
        .map_err(|error| {
            if is_foreign_key_violation(&error) {
                ApiError::conflict("snapshot is used by a sandbox environment template")
            } else {
                ApiError::database(error)
            }
        })?
        .then_some(StatusCode::NO_CONTENT)
        .ok_or_else(ApiError::not_found)
}

fn is_unique_violation(error: &DbError) -> bool {
    matches!(error, DbError::Sql(sqlx::Error::Database(error)) if error.is_unique_violation())
}

fn is_foreign_key_violation(error: &DbError) -> bool {
    matches!(error, DbError::Sql(sqlx::Error::Database(error)) if error.code().as_deref() == Some("23503"))
}
