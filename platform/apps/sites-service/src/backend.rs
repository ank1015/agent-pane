//! Invocation admission and durable receipts, separate from disposable JS execution.
use crate::{
    Error, Result, SiteService,
    bundles::{Bundle, Kind, digest},
    database, runtime,
};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::{
    collections::BTreeMap,
    sync::{Arc, atomic::AtomicBool},
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};
use uuid::Uuid;

#[derive(Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Request {
    pub method: String,
    pub path: String,
    #[serde(default)]
    pub query: BTreeMap<String, String>,
    #[serde(default)]
    pub body: Value,
}
fn default_timeout() -> u64 {
    10_000
}
#[derive(Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Invoke {
    pub id: Uuid,
    pub release_id: Option<Uuid>,
    pub request: Request,
    #[serde(default = "default_timeout")]
    pub timeout_ms: u64,
    /// Trusted Platform delivery metadata. The project-facing forwarding API
    /// does not accept this field; guests cannot manufacture invocation source.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub callback: Option<CallbackContext>,
}
#[derive(Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct CallbackContext {
    pub event_id: Uuid,
    pub subscription_id: Uuid,
}
#[derive(Serialize)]
pub struct Invocation {
    pub id: String,
    pub site_id: String,
    pub release_id: String,
    pub status: String,
    pub response: Option<Value>,
    pub error_code: Option<String>,
    pub logs: Value,
    pub created_at: String,
    pub finished_at: Option<String>,
}
#[derive(sqlx::FromRow)]
struct Row {
    id: String,
    site_id: String,
    release_id: String,
    status: String,
    response: Option<String>,
    error_code: Option<String>,
    logs: String,
    created_at: String,
    finished_at: Option<String>,
}
impl Row {
    fn record(self) -> Result<Invocation> {
        Ok(Invocation {
            id: self.id,
            site_id: self.site_id,
            release_id: self.release_id,
            status: self.status,
            response: self
                .response
                .map(|s| serde_json::from_str(&s))
                .transpose()
                .map_err(|_| Error::Storage)?,
            error_code: self.error_code,
            logs: serde_json::from_str(&self.logs).map_err(|_| Error::Storage)?,
            created_at: self.created_at,
            finished_at: self.finished_at,
        })
    }
}

impl SiteService {
    pub async fn invocation(&self, site: Uuid, id: Uuid) -> Result<Invocation> {
        self.get(site).await?;
        sqlx::query_as::<_,Row>("SELECT id,site_id,release_id,status,response,error_code,logs,created_at,finished_at FROM invocations WHERE site_id=? AND id=?")
            .bind(site.to_string()).bind(id.to_string()).fetch_optional(&self.0.pool).await?.ok_or(Error::ArtifactNotFound)?.record()
    }
    pub async fn invocations(&self, site: Uuid) -> Result<Vec<Invocation>> {
        self.get(site).await?;
        sqlx::query_as::<_,Row>("SELECT id,site_id,release_id,status,response,error_code,logs,created_at,finished_at FROM invocations WHERE site_id=? ORDER BY created_at DESC,id DESC LIMIT 100")
            .bind(site.to_string()).fetch_all(&self.0.pool).await?.into_iter().map(Row::record).collect()
    }
    pub async fn invoke(&self, site: Uuid, input: Invoke) -> Result<Invocation> {
        if !matches!(
            input.request.method.as_str(),
            "GET" | "POST" | "PUT" | "PATCH" | "DELETE" | "HEAD" | "OPTIONS"
        ) || !input.request.path.starts_with('/')
            || input.request.path.len() > 2048
            || input.request.path.chars().any(char::is_control)
            || input.request.path.contains(['?', '#'])
            || input.request.query.len() > 100
            || !(100..=30_000).contains(&input.timeout_ms)
        {
            return Err(Error::Invalid(
                "Invalid method, path, query, or timeout (100–30000 ms).",
            ));
        }
        let encoded = serde_json::to_vec(&input).map_err(|_| Error::Storage)?;
        if encoded.len() > 256 * 1024 {
            return Err(Error::Invalid("Invocation exceeds 256 KiB."));
        }
        let fingerprint = digest(&encoded);
        let permit = self
            .0
            .executions
            .clone()
            .try_acquire_owned()
            .map_err(|_| Error::Capacity)?;
        let service = self.clone();
        // Keep execution and admission alive after the HTTP caller disconnects.
        tokio::spawn(async move {
            let _permit = permit;
            let _site = service.site_gate(site).await?;
            let previous: Option<String> = sqlx::query_scalar("SELECT fingerprint FROM invocations WHERE site_id=? AND id=?")
                .bind(site.to_string()).bind(input.id.to_string()).fetch_optional(&service.0.pool).await?;
            if let Some(previous) = previous {
                if previous != fingerprint { return Err(Error::Conflict("IDEMPOTENCY_CONFLICT", "Invocation ID already identifies another request.")); }
                return service.invocation(site, input.id).await;
            }
            service.require_ready(site).await?;
            let record = service.get(site).await?;
            let project = Uuid::parse_str(&record.project_id).map_err(|_| Error::Storage)?;
            let release_id = input.release_id.or_else(|| record.active_release_id.and_then(|s| Uuid::parse_str(&s).ok()))
                .ok_or(Error::Conflict("NO_ACTIVE_RELEASE", "Publish and activate a release or supply release_id."))?;
            let release = service.bundle(site, Kind::Release, release_id).await?;
            service.check_release_schema(site, &release).await?;
            let code = service.read_bundle_file(site, Kind::Release, release_id, "backend.js".into()).await?;
            let code = String::from_utf8(code).map_err(|_| Error::Invalid("Backend must contain UTF-8 JavaScript."))?;
            let deadline = Instant::now() + Duration::from_millis(input.timeout_ms);
            let deadline_at = SystemTime::now().duration_since(UNIX_EPOCH).map_err(|_| Error::Storage)?.as_millis() as u64 + input.timeout_ms;
            let root = service.0.storage.root.clone();
            let conn = tokio::task::spawn_blocking(move || database::open(&root,site,project)).await??;
            let executable = std::env::current_exe()?;
            sqlx::query("INSERT INTO invocations(site_id,id,release_id,fingerprint,status) VALUES(?,?,?,?,'running')")
                .bind(site.to_string()).bind(input.id.to_string()).bind(release_id.to_string()).bind(fingerprint).execute(&service.0.pool).await?;
            let guest = runtime::GuestInput {
                code, timeout_ms:input.timeout_ms,
                context:json!({"request":input.request,"site":{"id":site,"projectId":project,"releaseId":release_id,"sdkVersion":"1"},
                    "invocation":{"id":input.id,"source":if input.callback.is_some(){"callback"}else{"internal"},
                        "eventId":input.callback.as_ref().map(|c|c.event_id),"subscriptionId":input.callback.as_ref().map(|c|c.subscription_id),"deadlineAt":deadline_at}}),
            };
            let db = database::Database::new(conn,deadline,Arc::new(AtomicBool::new(false)));
            let outcome = runtime::execute_with_platform(guest,db,&executable,service.0.platform.clone().map(|client| (client,crate::platform::Scope{site,project,invocation:input.id,release:release_id}))).await;
            let status = match outcome.error { None => "succeeded", Some("INVOCATION_TIMEOUT") => "timed_out", Some(_) => "failed" };
            sqlx::query("UPDATE invocations SET status=?,response=?,error_code=?,logs=?,finished_at=strftime('%Y-%m-%dT%H:%M:%fZ','now') WHERE site_id=? AND id=? AND status='running'")
                .bind(status).bind(outcome.response.map(|v| v.to_string())).bind(outcome.error)
                .bind(serde_json::to_string(&outcome.logs).map_err(|_| Error::Storage)?).bind(site.to_string()).bind(input.id.to_string()).execute(&service.0.pool).await?;
            service.invocation(site,input.id).await
        }).await?
    }
    pub(crate) async fn check_release_schema(&self, site: Uuid, release: &Bundle) -> Result<()> {
        if release.status != "ready" {
            return Err(Error::Conflict("BUNDLE_NOT_READY", "Release is not ready."));
        }
        let manifest = release.descriptor.manifest.as_ref().ok_or(Error::Storage)?;
        let range = manifest.schema.clone();
        let mut expected = Vec::new();
        if !manifest.migrations.is_empty() {
            let source = self
                .bundle(site, Kind::Revision, manifest.source_revision_id)
                .await?;
            for migration in &manifest.migrations {
                let file = source
                    .descriptor
                    .files
                    .get(&migration.path)
                    .ok_or(Error::Storage)?;
                expected.push((migration.version, file.sha256.clone()));
            }
        }
        let record = self.get(site).await?;
        let project = Uuid::parse_str(&record.project_id).map_err(|_| Error::Storage)?;
        let root = self.0.storage.root.clone();
        tokio::task::spawn_blocking(move || {
            let conn = database::open(&root,site,project)?;
            let version = database::version(&conn)?;
            for (number, checksum) in expected {
                if number > version { continue; }
                let actual = conn.query_row("SELECT checksum FROM __sites_migrations WHERE version=?", [number], |r| r.get::<_,String>(0)).map_err(|_| Error::Storage)?;
                if checksum != actual { return Err(Error::Conflict("MIGRATION_CONFLICT", "Release references different applied migration content.")); }
            }
            if version < range.min || version > range.max {
                return Err(Error::Conflict("SCHEMA_INCOMPATIBLE", "Release does not support the site's current schema. Apply migrations or select a compatible release."));
            }
            Ok(())
        }).await?
    }
}
