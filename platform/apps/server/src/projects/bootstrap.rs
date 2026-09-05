//! Read-only landing-page data. Accounts are safe summaries, never gateway credentials.
use axum::{
    Json, Router,
    extract::{Path, State, rejection::PathRejection},
    http::{HeaderValue, StatusCode, header::CACHE_CONTROL},
    middleware,
    response::{IntoResponse, Response},
    routing::get,
};
use platform_runtime_contracts::Harness;
use serde::Serialize;
use sqlx::PgPool;
use uuid::Uuid;

use super::{
    ProjectError,
    environments::{Environment, EnvironmentService},
};
use crate::providers::{ProviderAccountSummary, ProviderService};

#[derive(Clone)]
pub struct ProjectBootstrapService {
    pool: PgPool,
    providers: ProviderService,
    environments: EnvironmentService,
}

#[derive(Serialize)]
pub struct ProjectBootstrap {
    pub harnesses: Vec<Harness>,
    pub provider_accounts: Vec<ProviderAccountSummary>,
    pub project_environments: Vec<Environment>,
}

impl ProjectBootstrapService {
    pub fn new(pool: PgPool, providers: ProviderService, environments: EnvironmentService) -> Self {
        Self {
            pool,
            providers,
            environments,
        }
    }

    async fn load(&self, project_id: Uuid) -> Result<ProjectBootstrap, Response> {
        // Validate scope before making any outbound request.
        let exists: bool =
            sqlx::query_scalar("select exists(select 1 from projects where project_id=$1)")
                .bind(project_id)
                .fetch_one(&self.pool)
                .await
                .map_err(database_error)?;
        if !exists {
            return Err((
                StatusCode::NOT_FOUND,
                Json(serde_json::json!({"error":{
                    "code":"PROJECT_NOT_FOUND", "message":"Project not found."
                }})),
            )
                .into_response());
        }
        let (harnesses, accounts, environments) = tokio::try_join!(
            async {
                sqlx::query_scalar::<_, sqlx::types::Json<Harness>>(
                    "select to_jsonb(h) from harnesses h where enabled order by name,id",
                )
                .fetch_all(&self.pool)
                .await
                .map(|rows| rows.into_iter().map(|row| row.0).collect())
                .map_err(database_error)
            },
            async {
                self.providers
                    .list_accounts()
                    .await
                    .map_err(IntoResponse::into_response)
            },
            async {
                self.environments
                    .list(project_id)
                    .await
                    .map_err(IntoResponse::into_response)
            },
        )?;
        Ok(ProjectBootstrap {
            harnesses,
            provider_accounts: accounts,
            project_environments: environments,
        })
    }
}

fn database_error(error: sqlx::Error) -> Response {
    ProjectError::Database(error).into_response()
}

pub fn router(service: ProjectBootstrapService) -> Router {
    Router::new()
        .route("/api/projects/{project_id}/bootstrap", get(get_bootstrap))
        .layer(middleware::map_response(
            |mut response: Response| async move {
                response
                    .headers_mut()
                    .insert(CACHE_CONTROL, HeaderValue::from_static("no-store"));
                response
            },
        ))
        .with_state(service)
}

async fn get_bootstrap(
    State(service): State<ProjectBootstrapService>,
    path: Result<Path<Uuid>, PathRejection>,
) -> Result<Json<ProjectBootstrap>, Response> {
    let Path(id) = path.map_err(|_| {
        ProjectError::InvalidRequest("Project ID must be a valid UUID.").into_response()
    })?;
    Ok(Json(service.load(id).await?))
}
