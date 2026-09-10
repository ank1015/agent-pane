//! Authenticated project-facing site management; guest SDK is a separate route.
pub(crate) mod callbacks;
mod client;
mod http;
use crate::{providers::ProviderService, runtime::RuntimeService};
use axum::{
    Json,
    http::StatusCode,
    response::{IntoResponse, Response},
};
pub use client::SitesClient;
pub use http::router;
use serde_json::json;
use sha2::{Digest, Sha256};
use sqlx::PgPool;
use uuid::Uuid;

pub type Result<T> = std::result::Result<T, Error>;
#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("invalid site request")]
    Invalid(&'static str),
    #[error("unauthorized")]
    Unauthorized,
    #[error("not found")]
    NotFound,
    #[error("conflict")]
    Conflict,
    #[error("sites service unavailable")]
    Service(u16),
    /// Bounded public error envelope from the authenticated Sites service.
    #[error("sites service rejected the request")]
    Rejected {
        status: u16,
        error: platform_runtime_contracts::ErrorInfo,
    },
    #[error("database error")]
    Database(#[from] sqlx::Error),
    #[error("runtime error")]
    Runtime(#[from] crate::runtime::RuntimeError),
}
impl IntoResponse for Error {
    fn into_response(self) -> Response {
        let (status, code, message) = match self {
            Self::Rejected { status, error } => {
                return (
                    StatusCode::from_u16(status).unwrap_or(StatusCode::BAD_GATEWAY),
                    Json(platform_runtime_contracts::ErrorEnvelope { error }),
                )
                    .into_response();
            }
            Self::Invalid(m) => (400, "INVALID_SITE_REQUEST", m),
            Self::Unauthorized => (
                401,
                "SITE_UNAUTHORIZED",
                "Valid scoped site credentials are required.",
            ),
            Self::NotFound => (404, "SITE_NOT_FOUND", "Site or resource not found."),
            Self::Conflict => (
                409,
                "SITE_CONFLICT",
                "The request conflicts with existing site state.",
            ),
            Self::Service(404) => (
                404,
                "SITE_RESOURCE_NOT_FOUND",
                "The service resource was not found.",
            ),
            Self::Service(409) => (
                409,
                "SITE_SERVICE_CONFLICT",
                "The request conflicts with the site's release, schema, or invocation state.",
            ),
            Self::Service(400 | 413 | 415 | 422) => (
                400,
                "INVALID_SITE_SERVICE_REQUEST",
                "The sites service rejected this request.",
            ),
            Self::Service(_) => (
                502,
                "SITES_SERVICE_UNAVAILABLE",
                "The sites service could not complete the request. Inspect state before retrying.",
            ),
            Self::Database(_) => (
                503,
                "SITES_STORAGE_UNAVAILABLE",
                "Site metadata is unavailable. Retry using the same operation identity.",
            ),
            Self::Runtime(e) => return e.into_response(),
        };
        (
            StatusCode::from_u16(status).unwrap(),
            Json(json!({"error":{"code":code,"message":message}})),
        )
            .into_response()
    }
}
pub(crate) fn invalid(message: &'static str) -> Error {
    Error::Invalid(message)
}
pub(crate) fn upstream() -> Error {
    Error::Service(502)
}
pub(crate) fn token_hash(token: &str) -> String {
    format!("{:x}", Sha256::digest(token.as_bytes()))
}
pub(crate) fn validate_token(token: &str) -> Result<()> {
    if !(32..=256).contains(&token.len()) || !token.bytes().all(|b| b.is_ascii_graphic()) {
        return Err(invalid(
            "Tokens must contain 32–256 visible ASCII characters.",
        ));
    }
    Ok(())
}
#[derive(Clone)]
pub struct SitesService {
    pub(crate) pool: PgPool,
    pub(crate) client: SitesClient,
    pub(crate) runtime: RuntimeService,
    pub(crate) providers: ProviderService,
    pub(crate) capability_hash: String,
    pub(crate) admin_hash: Option<String>,
}
impl SitesService {
    pub fn new(
        pool: PgPool,
        client: SitesClient,
        runtime: RuntimeService,
        providers: ProviderService,
        capability_token: &str,
        admin_token: &str,
    ) -> Result<Self> {
        validate_token(capability_token)?;
        if !admin_token.is_empty() {
            validate_token(admin_token)?;
        }
        if capability_token == admin_token {
            return Err(invalid("Capability and admin tokens must differ."));
        }
        Ok(Self {
            pool,
            client,
            runtime,
            providers,
            capability_hash: token_hash(capability_token),
            admin_hash: (!admin_token.is_empty()).then(|| token_hash(admin_token)),
        })
    }
    // No mutable/global scope is shared between invocations.
    pub(crate) async fn site(&self, project: Uuid, site: Uuid) -> Result<serde_json::Value> {
        sqlx::query_scalar("select to_jsonb(s) from project_sites s where id=$1 and project_id=$2 and deleted_at is null")
            .bind(site).bind(project).fetch_optional(&self.pool).await?.ok_or(Error::NotFound)
    }
    pub(crate) async fn reconcile(&self, project: Uuid, site: Uuid) -> Result<serde_json::Value> {
        self.site(project, site).await?;
        self.client
            .call(
                reqwest::Method::PUT,
                &format!("/internal/sites/{site}"),
                Some(&json!({"project_id":project})),
            )
            .await?;
        // Refresh intent after provisioning; no database lock is held across HTTP.
        let record = self.site(project, site).await?;
        self.client
            .call(
                reqwest::Method::PATCH,
                &format!("/internal/sites/{site}"),
                Some(&json!({"status":record["desired_status"]})),
            )
            .await
    }
    pub fn spawn_reconciler(&self) -> tokio::task::JoinHandle<()> {
        let service = self.clone();
        tokio::spawn(async move {
            loop {
                let mut after = Uuid::nil();
                loop {
                    let rows = sqlx::query_as::<_, (Uuid, Uuid)>(
                        "select id,project_id from project_sites where id>$1 order by id limit 100",
                    )
                    .bind(after)
                    .fetch_all(&service.pool)
                    .await;
                    let Ok(rows) = rows else { break };
                    if rows.is_empty() {
                        break;
                    }
                    for (site, project) in rows {
                        after = site;
                        // Deleted records are tombstones; keep their physical resources suspended.
                        let deleted: bool = sqlx::query_scalar(
                            "select deleted_at is not null from project_sites where id=$1",
                        )
                        .bind(site)
                        .fetch_one(&service.pool)
                        .await
                        .unwrap_or(true);
                        if deleted {
                            let _ = service
                                .client
                                .call(
                                    reqwest::Method::PATCH,
                                    &format!("/internal/sites/{site}"),
                                    Some(&json!({"status":"suspended"})),
                                )
                                .await;
                        } else {
                            let _ = service.reconcile(project, site).await;
                        }
                    }
                }
                tokio::time::sleep(std::time::Duration::from_secs(10)).await;
            }
        })
    }
}
