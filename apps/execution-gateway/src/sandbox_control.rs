use axum::{
    Json, Router,
    extract::{Path, State},
    http::{HeaderMap, StatusCode},
    routing::get,
};
use execution_contracts::MachineId;
use execution_protocol::{
    CreateSandboxMachineRequest, MachineSummary, SandboxMachineCreated, SandboxSnapshotCreated,
};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::{
    http::{ApiError, AppState, require, validate_machine_name},
    sandbox_accounts::SandboxProvider,
    sandbox_machines::SandboxMachineSummary,
    sandbox_management::CreatedSandboxMachine,
    snapshots::Snapshot,
};

pub(crate) fn routes() -> Router<AppState> {
    Router::new()
        .route(
            "/v1/sandbox-accounts/{account_id}/sandboxes",
            axum::routing::post(create_api_sandbox),
        )
        .route(
            "/v1/machines/{machine_id}/snapshots",
            axum::routing::post(create_api_snapshot),
        )
        .route(
            "/v1/control/sandbox-accounts/{account_id}/sandboxes",
            get(list_sandboxes).post(create_sandbox),
        )
        .route(
            "/v1/control/sandbox-accounts/{account_id}/sandboxes/{sandbox_id}/snapshots",
            axum::routing::post(create_snapshot),
        )
}

async fn create_api_snapshot(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(machine_id): Path<String>,
) -> Result<(StatusCode, Json<SandboxSnapshotCreated>), ApiError> {
    require(&headers, &state.api_token)?;
    let machine_id = MachineId::new(machine_id)
        .map_err(|error| ApiError::bad(format!("invalid machine_id: {error}")))?;
    state
        .sandbox_materializer
        .create_sandbox_snapshot_for_machine(&machine_id)
        .await
        .map(|snapshot| {
            (
                StatusCode::CREATED,
                Json(SandboxSnapshotCreated {
                    snapshot_id: snapshot.id,
                }),
            )
        })
        .map_err(ApiError::sandbox_materialization)
}

async fn create_api_sandbox(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(account_id): Path<Uuid>,
    Json(request): Json<CreateSandboxMachineRequest>,
) -> Result<(StatusCode, Json<SandboxMachineCreated>), ApiError> {
    require(&headers, &state.api_token)?;
    let created = match request.snapshot_id {
        Some(snapshot_id) => {
            state
                .sandbox_materializer
                .create_sandbox_from_snapshot(account_id, snapshot_id, None)
                .await
        }
        None => {
            let account = state
                .sandbox_accounts
                .find(account_id)
                .await
                .map_err(ApiError::sandbox_account)?
                .ok_or_else(ApiError::not_found)?;
            let source = creation_source(account.provider, None)?;
            state
                .sandbox_materializer
                .create_sandbox(account_id, source, None)
                .await
        }
    }
    .map_err(ApiError::sandbox_materialization)?;
    Ok((
        StatusCode::CREATED,
        Json(SandboxMachineCreated {
            machine: created.machine,
        }),
    ))
}

#[derive(Debug, Deserialize)]
pub struct CreateSandboxRequest {
    #[serde(default)]
    pub template_id: Option<String>,
    #[serde(default)]
    pub name: Option<String>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct SandboxCreated {
    pub machine: MachineSummary,
    pub sandbox_account_id: Uuid,
    pub sandbox_id: String,
    pub created_from: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct CreateSandboxSnapshotRequest {
    pub name: String,
}

async fn list_sandboxes(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(account_id): Path<Uuid>,
) -> Result<Json<Vec<SandboxMachineSummary>>, ApiError> {
    require(&headers, &state.control_token)?;
    state
        .sandbox_accounts
        .find(account_id)
        .await
        .map_err(ApiError::sandbox_account)?
        .ok_or_else(ApiError::not_found)?;
    state
        .database
        .sandbox_machines_for_account(account_id)
        .await
        .map(Json)
        .map_err(ApiError::database)
}

async fn create_sandbox(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(account_id): Path<Uuid>,
    Json(request): Json<CreateSandboxRequest>,
) -> Result<(StatusCode, Json<SandboxCreated>), ApiError> {
    require(&headers, &state.control_token)?;
    if let Some(name) = request.name.as_deref() {
        validate_machine_name(name)?;
    }
    let account = state
        .sandbox_accounts
        .find(account_id)
        .await
        .map_err(ApiError::sandbox_account)?
        .ok_or_else(ApiError::not_found)?;
    let source = creation_source(account.provider, request.template_id.as_deref())?;
    state
        .sandbox_materializer
        .create_sandbox(account_id, source, request.name.as_deref())
        .await
        .map(SandboxCreated::from)
        .map(|sandbox| (StatusCode::CREATED, Json(sandbox)))
        .map_err(ApiError::sandbox_materialization)
}

async fn create_snapshot(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path((account_id, sandbox_id)): Path<(Uuid, String)>,
    Json(request): Json<CreateSandboxSnapshotRequest>,
) -> Result<(StatusCode, Json<Snapshot>), ApiError> {
    require(&headers, &state.control_token)?;
    validate_snapshot_name(&request.name)?;
    state
        .sandbox_materializer
        .create_sandbox_snapshot(account_id, &sandbox_id, &request.name)
        .await
        .map(|snapshot| (StatusCode::CREATED, Json(snapshot)))
        .map_err(ApiError::sandbox_materialization)
}

fn creation_source(
    provider: SandboxProvider,
    template_id: Option<&str>,
) -> Result<Option<&str>, ApiError> {
    match provider {
        SandboxProvider::E2b => {
            let template_id = template_id.unwrap_or("base");
            if template_id.is_empty() || template_id != template_id.trim() {
                return Err(ApiError::bad(
                    "template_id must not be blank or have surrounding whitespace",
                ));
            }
            Ok(Some(template_id))
        }
        SandboxProvider::Daytona | SandboxProvider::Blaxel | SandboxProvider::Tensorlake
            if template_id.is_none() =>
        {
            Ok(None)
        }
        SandboxProvider::Daytona | SandboxProvider::Blaxel | SandboxProvider::Tensorlake => {
            Err(ApiError::bad(format!(
                "{provider} sandbox creation does not accept template_id"
            )))
        }
    }
}

fn validate_snapshot_name(name: &str) -> Result<(), ApiError> {
    if name.is_empty() || name != name.trim() || name.chars().count() > 120 {
        return Err(ApiError::bad(
            "snapshot name must be 1-120 characters without surrounding whitespace",
        ));
    }
    Ok(())
}

impl From<CreatedSandboxMachine> for SandboxCreated {
    fn from(created: CreatedSandboxMachine) -> Self {
        Self {
            machine: created.machine,
            sandbox_account_id: created.sandbox_account_id,
            sandbox_id: created.sandbox_id,
            created_from: created.created_from,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn provider_creation_inputs_are_distinct() {
        assert_eq!(
            creation_source(SandboxProvider::E2b, None).ok().flatten(),
            Some("base")
        );
        assert_eq!(
            creation_source(SandboxProvider::Daytona, None)
                .ok()
                .flatten(),
            None
        );
        assert!(creation_source(SandboxProvider::Daytona, Some("base")).is_err());
        assert_eq!(
            creation_source(SandboxProvider::Blaxel, None)
                .ok()
                .flatten(),
            None
        );
        assert!(creation_source(SandboxProvider::Blaxel, Some("base")).is_err());
        assert_eq!(
            creation_source(SandboxProvider::Tensorlake, None)
                .ok()
                .flatten(),
            None
        );
        assert!(creation_source(SandboxProvider::Tensorlake, Some("base")).is_err());
    }

    #[test]
    fn validates_snapshot_names() {
        assert!(validate_snapshot_name("Ready workspace").is_ok());
        assert!(validate_snapshot_name("").is_err());
        assert!(validate_snapshot_name(" Ready workspace").is_err());
        assert!(validate_snapshot_name(&"x".repeat(121)).is_err());
    }
}
