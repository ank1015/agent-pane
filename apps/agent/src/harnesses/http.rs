use axum::{
    Json, Router,
    extract::{
        Path, Query, State,
        rejection::{JsonRejection, PathRejection, QueryRejection},
    },
    http::{HeaderValue, StatusCode, header::CACHE_CONTROL},
    response::{IntoResponse, Response},
    routing::{get, post, put},
};
use serde::Serialize;
use serde_json::json;

use super::{
    CreateHarness, HarnessError, HarnessListQuery, RegisterHarnessRevision, RevisionListQuery,
    SetActiveRevision, SetHarnessEnabled, UpdateHarness, definitions, revisions,
    types::validate_path_id,
};
use crate::{api_error::ApiError, app::AppState};

pub fn router() -> Router<AppState> {
    Router::new()
        .route("/v1/harnesses", get(list_harnesses).post(create_harness))
        .route(
            "/v1/harnesses/{harness_id}",
            get(get_harness).patch(update_harness),
        )
        .route(
            "/v1/harnesses/{harness_id}/enabled",
            put(set_harness_enabled),
        )
        .route(
            "/v1/harnesses/{harness_id}/revisions",
            get(list_revisions).post(register_revision),
        )
        .route(
            "/v1/harnesses/{harness_id}/revisions/{revision_id}",
            get(get_revision),
        )
        .route(
            "/v1/harnesses/{harness_id}/active-revision",
            put(activate_revision).delete(clear_active_revision),
        )
        .route(
            "/v1/harnesses/{harness_id}/revisions/{revision_id}/retire",
            post(retire_revision),
        )
}

async fn create_harness(
    State(state): State<AppState>,
    payload: Result<Json<CreateHarness>, JsonRejection>,
) -> Result<Response, ApiError> {
    let outcome = definitions::create(state.database(), json_body(payload)?).await?;
    let status = if outcome.created {
        StatusCode::CREATED
    } else {
        StatusCode::OK
    };
    Ok(json_response(status, outcome.value))
}

async fn list_harnesses(
    State(state): State<AppState>,
    query: Result<Query<HarnessListQuery>, QueryRejection>,
) -> Result<Response, ApiError> {
    let query = query
        .map(|Query(query)| query)
        .map_err(|error| ApiError::invalid_request(error.body_text()))?;
    let page = definitions::list(state.database(), query).await?;
    Ok(json_response(StatusCode::OK, page))
}

async fn get_harness(
    State(state): State<AppState>,
    path: Result<Path<String>, PathRejection>,
) -> Result<Response, ApiError> {
    let harness_id = harness_path(path)?;
    let harness = definitions::get(state.database(), &harness_id).await?;
    Ok(json_response(StatusCode::OK, harness))
}

async fn update_harness(
    State(state): State<AppState>,
    path: Result<Path<String>, PathRejection>,
    payload: Result<Json<UpdateHarness>, JsonRejection>,
) -> Result<Response, ApiError> {
    let harness_id = harness_path(path)?;
    let harness = definitions::update(state.database(), &harness_id, json_body(payload)?).await?;
    Ok(json_response(StatusCode::OK, harness))
}

async fn set_harness_enabled(
    State(state): State<AppState>,
    path: Result<Path<String>, PathRejection>,
    payload: Result<Json<SetHarnessEnabled>, JsonRejection>,
) -> Result<Response, ApiError> {
    let harness_id = harness_path(path)?;
    let harness =
        definitions::set_enabled(state.database(), &harness_id, json_body(payload)?).await?;
    Ok(json_response(StatusCode::OK, harness))
}

async fn register_revision(
    State(state): State<AppState>,
    path: Result<Path<String>, PathRejection>,
    payload: Result<Json<RegisterHarnessRevision>, JsonRejection>,
) -> Result<Response, ApiError> {
    let harness_id = harness_path(path)?;
    let outcome = revisions::register(state.database(), &harness_id, json_body(payload)?).await?;
    let status = if outcome.created {
        StatusCode::CREATED
    } else {
        StatusCode::OK
    };
    Ok(json_response(status, outcome.value))
}

async fn list_revisions(
    State(state): State<AppState>,
    path: Result<Path<String>, PathRejection>,
    query: Result<Query<RevisionListQuery>, QueryRejection>,
) -> Result<Response, ApiError> {
    let harness_id = harness_path(path)?;
    let query = query
        .map(|Query(query)| query)
        .map_err(|error| ApiError::invalid_request(error.body_text()))?;
    let page = revisions::list(state.database(), &harness_id, query).await?;
    Ok(json_response(StatusCode::OK, page))
}

async fn get_revision(
    State(state): State<AppState>,
    path: Result<Path<(String, String)>, PathRejection>,
) -> Result<Response, ApiError> {
    let (harness_id, revision_id) = revision_path(path)?;
    let revision = revisions::get(state.database(), &harness_id, &revision_id).await?;
    Ok(json_response(StatusCode::OK, revision))
}

async fn activate_revision(
    State(state): State<AppState>,
    path: Result<Path<String>, PathRejection>,
    payload: Result<Json<SetActiveRevision>, JsonRejection>,
) -> Result<Response, ApiError> {
    let harness_id = harness_path(path)?;
    let activation =
        revisions::activate(state.database(), &harness_id, json_body(payload)?).await?;
    Ok(json_response(StatusCode::OK, activation))
}

async fn clear_active_revision(
    State(state): State<AppState>,
    path: Result<Path<String>, PathRejection>,
) -> Result<Response, ApiError> {
    let harness_id = harness_path(path)?;
    let harness = revisions::clear_active(state.database(), &harness_id).await?;
    Ok(json_response(StatusCode::OK, harness))
}

async fn retire_revision(
    State(state): State<AppState>,
    path: Result<Path<(String, String)>, PathRejection>,
) -> Result<Response, ApiError> {
    let (harness_id, revision_id) = revision_path(path)?;
    let revision = revisions::retire(state.database(), &harness_id, &revision_id).await?;
    Ok(json_response(StatusCode::OK, revision))
}

fn json_body<T>(payload: Result<Json<T>, JsonRejection>) -> Result<T, ApiError> {
    payload
        .map(|Json(value)| value)
        .map_err(|error| ApiError::invalid_request(error.body_text()))
}

fn harness_path(path: Result<Path<String>, PathRejection>) -> Result<String, ApiError> {
    let Path(harness_id) = path.map_err(|error| ApiError::invalid_request(error.body_text()))?;
    validate_path_id("harness_id", &harness_id).map_err(HarnessError::from)?;
    Ok(harness_id)
}

fn revision_path(
    path: Result<Path<(String, String)>, PathRejection>,
) -> Result<(String, String), ApiError> {
    let Path((harness_id, revision_id)) =
        path.map_err(|error| ApiError::invalid_request(error.body_text()))?;
    validate_path_id("harness_id", &harness_id).map_err(HarnessError::from)?;
    validate_path_id("revision_id", &revision_id).map_err(HarnessError::from)?;
    Ok((harness_id, revision_id))
}

fn json_response<T: Serialize>(status: StatusCode, body: T) -> Response {
    let mut response = (status, Json(body)).into_response();
    response
        .headers_mut()
        .insert(CACHE_CONTROL, HeaderValue::from_static("no-store"));
    response
}

impl From<HarnessError> for ApiError {
    fn from(error: HarnessError) -> Self {
        match error {
            HarnessError::Validation(error) => ApiError::invalid_request(error.to_string())
                .with_details(json!({ "issues": error.issues })),
            HarnessError::HarnessNotFound(_) => ApiError::new(
                StatusCode::NOT_FOUND,
                "harness_not_found",
                error.to_string(),
            ),
            HarnessError::RevisionNotFound(_) => ApiError::new(
                StatusCode::NOT_FOUND,
                "harness_revision_not_found",
                error.to_string(),
            ),
            HarnessError::HarnessIdConflict(_) => conflict("harness_id_conflict", error),
            HarnessError::HarnessSlugConflict(_) => conflict("harness_slug_conflict", error),
            HarnessError::RevisionIdConflict(_) => conflict("harness_revision_id_conflict", error),
            HarnessError::RevisionNameConflict { .. } => {
                conflict("harness_revision_name_conflict", error)
            }
            HarnessError::NoActiveRevision => conflict("no_active_revision", error),
            HarnessError::EnabledHarnessCannotClearRevision => conflict("harness_enabled", error),
            HarnessError::ActiveRevisionCannotBeRetired => {
                conflict("active_revision_cannot_be_retired", error)
            }
            HarnessError::RetiredRevisionCannotBeActivated => {
                conflict("retired_revision_cannot_be_activated", error)
            }
            HarnessError::InvalidPageSize | HarnessError::InvalidCursor => {
                ApiError::invalid_request(error.to_string())
            }
            HarnessError::InvalidConfigSchema(_) => ApiError::new(
                StatusCode::UNPROCESSABLE_ENTITY,
                "invalid_config_schema",
                error.to_string(),
            ),
            HarnessError::DefaultConfigInvalid(_) => ApiError::new(
                StatusCode::UNPROCESSABLE_ENTITY,
                "default_config_invalid",
                error.to_string(),
            ),
            HarnessError::InvalidStoredData(_) | HarnessError::Database(_) => {
                tracing::error!(%error, "harness request failed");
                ApiError::internal()
            }
        }
    }
}

fn conflict(code: &'static str, error: HarnessError) -> ApiError {
    ApiError::new(StatusCode::CONFLICT, code, error.to_string())
}
