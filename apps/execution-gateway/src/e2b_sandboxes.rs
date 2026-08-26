use axum::{
    Json, Router,
    extract::{Path, State},
    http::{HeaderMap, StatusCode},
    routing::get,
};
use execution_protocol::MachineSummary;
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::{
    http::{ApiError, AppState, require, validate_machine_name},
    sandbox_accounts::{SandboxAccountError, SandboxProvider},
    sandbox_machines::SandboxMachineSummary,
    sandbox_materialization::CreatedE2bSandboxMachine,
    snapshots::Snapshot,
};

pub(crate) fn routes() -> Router<AppState> {
    Router::new()
        .route(
            "/v1/control/sandbox-accounts/{account_id}/sandboxes",
            get(list_e2b_sandboxes).post(create_e2b_sandbox),
        )
        .route(
            "/v1/control/sandbox-accounts/{account_id}/sandboxes/{sandbox_id}/snapshots",
            axum::routing::post(create_e2b_snapshot),
        )
}

#[derive(Debug, Deserialize)]
pub struct CreateE2bSandboxRequest {
    pub template_id: String,
    #[serde(default)]
    pub name: Option<String>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct E2bSandboxCreated {
    pub machine: MachineSummary,
    pub sandbox_account_id: Uuid,
    pub sandbox_id: String,
    pub template_id: String,
}

#[derive(Debug, Deserialize)]
pub struct CreateE2bSnapshotRequest {
    pub name: String,
}

async fn list_e2b_sandboxes(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(account_id): Path<Uuid>,
) -> Result<Json<Vec<SandboxMachineSummary>>, ApiError> {
    require(&headers, &state.control_token)?;
    let account = state
        .sandbox_accounts
        .find(account_id)
        .await
        .map_err(ApiError::sandbox_account)?
        .ok_or_else(ApiError::not_found)?;
    if account.provider != SandboxProvider::E2b {
        return Err(ApiError::sandbox_account(
            SandboxAccountError::ProviderMismatch,
        ));
    }
    state
        .database
        .sandbox_machines_for_account(account_id)
        .await
        .map(Json)
        .map_err(ApiError::database)
}

async fn create_e2b_sandbox(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(account_id): Path<Uuid>,
    Json(request): Json<CreateE2bSandboxRequest>,
) -> Result<(StatusCode, Json<E2bSandboxCreated>), ApiError> {
    require(&headers, &state.control_token)?;
    validate_create_sandbox_request(&request)?;
    state
        .sandbox_materializer
        .create_e2b_sandbox(account_id, &request.template_id, request.name.as_deref())
        .await
        .map(E2bSandboxCreated::from)
        .map(|sandbox| (StatusCode::CREATED, Json(sandbox)))
        .map_err(ApiError::sandbox_materialization)
}

async fn create_e2b_snapshot(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path((account_id, sandbox_id)): Path<(Uuid, String)>,
    Json(request): Json<CreateE2bSnapshotRequest>,
) -> Result<(StatusCode, Json<Snapshot>), ApiError> {
    require(&headers, &state.control_token)?;
    validate_snapshot_name(&request.name)?;
    state
        .sandbox_materializer
        .create_e2b_snapshot(account_id, &sandbox_id, &request.name)
        .await
        .map(|snapshot| (StatusCode::CREATED, Json(snapshot)))
        .map_err(ApiError::sandbox_materialization)
}

fn validate_create_sandbox_request(request: &CreateE2bSandboxRequest) -> Result<(), ApiError> {
    if request.template_id.is_empty() || request.template_id != request.template_id.trim() {
        return Err(ApiError::bad(
            "template_id must not be blank or have surrounding whitespace",
        ));
    }
    if let Some(name) = request.name.as_deref() {
        validate_machine_name(name)?;
    }
    Ok(())
}

fn validate_snapshot_name(name: &str) -> Result<(), ApiError> {
    if name.is_empty() || name != name.trim() || name.chars().count() > 120 {
        return Err(ApiError::bad(
            "snapshot name must be 1-120 characters without surrounding whitespace",
        ));
    }
    Ok(())
}

impl From<CreatedE2bSandboxMachine> for E2bSandboxCreated {
    fn from(created: CreatedE2bSandboxMachine) -> Self {
        Self {
            machine: created.machine,
            sandbox_account_id: created.sandbox_account_id,
            sandbox_id: created.sandbox_id,
            template_id: created.template_id,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn validates_creation_input() {
        let valid = CreateE2bSandboxRequest {
            template_id: "base".to_owned(),
            name: None,
        };
        assert!(validate_create_sandbox_request(&valid).is_ok());

        let invalid_template = CreateE2bSandboxRequest {
            template_id: " base".to_owned(),
            name: None,
        };
        assert!(validate_create_sandbox_request(&invalid_template).is_err());

        let invalid_name = CreateE2bSandboxRequest {
            template_id: "base".to_owned(),
            name: Some(String::new()),
        };
        assert!(validate_create_sandbox_request(&invalid_name).is_err());
    }

    #[test]
    fn validates_snapshot_names() {
        assert!(validate_snapshot_name("Ready workspace").is_ok());
        assert!(validate_snapshot_name("").is_err());
        assert!(validate_snapshot_name(" Ready workspace").is_err());
        assert!(validate_snapshot_name(&"x".repeat(121)).is_err());
    }
}
