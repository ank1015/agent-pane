//! Direct editing with frozen retry inputs and atomic code activation.
use crate::{Error, Result, SiteService, bundles::*};
use base64::{Engine as _, engine::general_purpose::STANDARD};
use platform_runtime_contracts::sites_authoring::{Action, Files, Operation, Source};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::{io::Read, path::Path, time::Duration};
use tokio::io::AsyncWriteExt;
use uuid::Uuid;
const FILE_BYTES: usize = 48 * 1024;
const FRONTEND: &str = "<!doctype html>\n<html><head><meta charset=\"utf-8\"><meta name=\"viewport\" content=\"width=device-width,initial-scale=1\"><title>New site</title></head><body><main><h1>New site</h1></main></body></html>\n";
const BACKEND: &str = "export default async function handle(request, ctx) {\n  return {status: 200, body: {message: 'New site'}};\n}\n";
#[derive(Serialize, Deserialize)]
struct Prepared {
    generation: i64,
    release: Uuid,
    revision: Uuid,
    files: Option<Files>,
    schema: SchemaRange,
    #[serde(default)]
    migrations: Vec<MigrationFile>,
    #[serde(default)]
    migration_files: Vec<FileUpload>,
}
fn invalid() -> Error {
    Error::Invalid(
        "Only index.html and backend.js may be updated; use matching patch context and keep each file within 48 KiB.",
    )
}
fn upload(path: &str, content: &str) -> FileUpload {
    FileUpload {
        path: path.into(),
        content_base64: STANDARD.encode(content),
        sha256: digest(content.as_bytes()),
    }
}
impl SiteService {
    pub async fn authoring_source(&self, site: Uuid) -> Result<Source> {
        let record = self.get(site).await?;
        let Some(id) = record.active_release_id else {
            return Ok(Source {
                site_id: site,
                release_id: None,
                files: Files {
                    frontend: FRONTEND.into(),
                    backend: BACKEND.into(),
                },
            });
        };
        let id = Uuid::parse_str(&id).map_err(|_| Error::Storage)?;
        let bundle = self.bundle(site, Kind::Release, id).await?;
        let manifest = bundle.descriptor.manifest.ok_or(Error::Storage)?;
        if bundle.descriptor.files.len() != 2
            || manifest.frontend_entrypoint != "public/index.html"
            || manifest.backend_entrypoint != "backend.js"
        {
            return Err(Error::Conflict(
                "AUTHORING_FORMAT_UNSUPPORTED",
                "This release cannot be edited as two files without discarding assets.",
            ));
        }
        let mut files = Files {
            frontend: String::new(),
            backend: String::new(),
        };
        for (path, value) in [
            ("public/index.html", &mut files.frontend),
            ("backend.js", &mut files.backend),
        ] {
            if bundle
                .descriptor
                .files
                .get(path)
                .is_none_or(|f| f.size > FILE_BYTES)
            {
                return Err(invalid());
            }
            *value = String::from_utf8(
                self.read_bundle_file(site, Kind::Release, id, path.into())
                    .await?,
            )
            .map_err(|_| invalid())?;
        }
        Ok(Source {
            site_id: site,
            files,
            release_id: Some(id),
        })
    }
    pub async fn authoring_operation(&self, site: Uuid, id: Uuid) -> Result<Value> {
        self.get(site).await?;
        let row: Option<Option<String>> = sqlx::query_scalar(
            "SELECT response FROM authoring_operations WHERE site_id=? AND id=?",
        )
        .bind(site.to_string())
        .bind(id.to_string())
        .fetch_optional(&self.0.pool)
        .await?;
        match row {
            Some(Some(s)) => serde_json::from_str(&s).map_err(|_| Error::Storage),
            Some(None) => Ok(json!({"id":id,"siteId":site,"status":"pending"})),
            None => Err(Error::ArtifactNotFound),
        }
    }
    pub async fn snapshots(&self, site: Uuid, after: Option<Uuid>, limit: u32) -> Result<Value> {
        self.get(site).await?;
        if !(1..=50).contains(&limit) {
            return Err(Error::Invalid("Snapshot page limit must be 1–50."));
        }
        let rows:Vec<(String,String,String,String)>=sqlx::query_as("SELECT id,name,release_id,created_at FROM site_snapshots WHERE site_id=? AND id>? ORDER BY id LIMIT ?").bind(site.to_string()).bind(after.map(|x|x.to_string()).unwrap_or_default()).bind(i64::from(limit)+1).fetch_all(&self.0.pool).await?;
        let more = rows.len() > limit as usize;
        let items:Vec<Value>=rows.into_iter().take(limit as usize).map(|(id,name,release,created)|json!({"id":id,"name":name,"release_id":release,"created_at":created})).collect();
        let next = if more {
            items.last().map(|x| x["id"].clone())
        } else {
            None
        };
        Ok(json!({"items":items,"nextAfter":next}))
    }
    /// Accepted work outlives the HTTP request. Replay never rebuilds a patch
    /// against a newer base; the prepared files and generation are durable.
    pub async fn author(&self, site: Uuid, input: Operation, executable: &Path) -> Result<Value> {
        if input.id.is_nil() {
            return Err(Error::Invalid("A stable operation UUID is required."));
        }
        let permit = self
            .0
            .executions
            .clone()
            .try_acquire_owned()
            .map_err(|_| Error::Capacity)?;
        let service = self.clone();
        let executable = executable.to_owned();
        tokio::spawn(async move {
            let _permit = permit;
            let _edit = service.0.authoring.lock().await;
            service.author_inner(site, input, &executable).await
        })
        .await?
    }
    async fn author_inner(&self, site: Uuid, input: Operation, executable: &Path) -> Result<Value> {
        let request = serde_json::to_string(&input).map_err(|_| Error::Storage)?;
        let saved: Option<(String, String, Option<String>)> = sqlx::query_as(
            "SELECT request,prepared,response FROM authoring_operations WHERE site_id=? AND id=?",
        )
        .bind(site.to_string())
        .bind(input.id.to_string())
        .fetch_optional(&self.0.pool)
        .await?;
        let prepared: Prepared = if let Some((old, p, response)) = saved {
            if old != request {
                return Err(Error::Conflict(
                    "IDEMPOTENCY_CONFLICT",
                    "Operation ID has different input.",
                ));
            }
            if let Some(response) = response {
                return serde_json::from_str(&response).map_err(|_| Error::Storage);
            }
            serde_json::from_str(&p).map_err(|_| Error::Storage)?
        } else {
            let record = self.get(site).await?;
            if record.status != crate::Status::Ready {
                return Err(Error::Conflict(
                    "SITE_NOT_READY",
                    "Site must be ready for authoring.",
                ));
            }
            let mut p = Prepared {
                generation: record.release_generation,
                release: Uuid::now_v7(),
                revision: Uuid::now_v7(),
                files: None,
                schema: SchemaRange::default(),
                migrations: vec![],
                migration_files: vec![],
            };
            match &input.action {
                Action::Patch { patch } => {
                    if patch.len() > 48 * 1024 {
                        return Err(invalid());
                    }
                    let parsed = tool_apply_patch::parse_patch(patch).map_err(|_| invalid())?;
                    if parsed.environment_id.is_some()
                        || parsed.hunks.is_empty()
                        || parsed.hunks.len() > 64
                    {
                        return Err(invalid());
                    }
                    let source = self.authoring_source(site).await?;
                    if source.release_id.map(|x| x.to_string()) != record.active_release_id {
                        return Err(Error::Conflict(
                            "AUTHORING_CONFLICT",
                            "Site changed; read and patch again.",
                        ));
                    }
                    if let Some(id) = source.release_id {
                        let manifest = self
                            .bundle(site, Kind::Release, id)
                            .await?
                            .descriptor
                            .manifest
                            .ok_or(Error::Storage)?;
                        p.schema = manifest.schema;
                        let mut total = 0;
                        for migration in &manifest.migrations {
                            let bytes = self
                                .read_bundle_file(
                                    site,
                                    Kind::Revision,
                                    manifest.source_revision_id,
                                    migration.path.clone(),
                                )
                                .await?;
                            total += bytes.len();
                            if total > 1024 * 1024 {
                                return Err(Error::Invalid(
                                    "Preserved migration history exceeds 1 MiB.",
                                ));
                            }
                            p.migration_files.push(FileUpload {
                                path: migration.path.clone(),
                                sha256: digest(&bytes),
                                content_base64: STANDARD.encode(bytes),
                            });
                        }
                        p.migrations = manifest.migrations;
                    }
                    let mut files = source.files;
                    for hunk in parsed.hunks {
                        let tool_apply_patch::Hunk::UpdateFile {
                            path,
                            move_path: None,
                            chunks,
                        } = hunk
                        else {
                            return Err(invalid());
                        };
                        let file = match path.as_str() {
                            "index.html" => &mut files.frontend,
                            "backend.js" => &mut files.backend,
                            _ => return Err(invalid()),
                        };
                        *file = tool_apply_patch::derive_new_contents(
                            &path,
                            file,
                            &chunks,
                            tool_apply_patch::ApplyPatchFileUpdateMode::NormalizeToLf,
                        )
                        .map_err(|_| invalid())?;
                        if file.trim().is_empty() || file.len() > FILE_BYTES {
                            return Err(invalid());
                        }
                    }
                    if json!(files).to_string().len() > 96 * 1024 {
                        return Err(invalid());
                    }
                    validate_backend(&files.backend, executable).await?;
                    p.files = Some(files);
                }
                Action::Snapshot { name } => {
                    if name.trim() != name
                        || name.is_empty()
                        || name.len() > 128
                        || name.chars().any(char::is_control)
                    {
                        return Err(Error::Invalid(
                            "Provide a trimmed snapshot name of 1–128 bytes.",
                        ));
                    }
                    p.release = record
                        .active_release_id
                        .and_then(|s| Uuid::parse_str(&s).ok())
                        .ok_or(Error::Conflict(
                            "NO_ACTIVE_CODE",
                            "Edit the starter files before saving a snapshot.",
                        ))?;
                }
                Action::Restore { snapshot_id } => {
                    let release: String = sqlx::query_scalar(
                        "SELECT release_id FROM site_snapshots WHERE site_id=? AND id=?",
                    )
                    .bind(site.to_string())
                    .bind(snapshot_id.to_string())
                    .fetch_optional(&self.0.pool)
                    .await?
                    .ok_or(Error::ArtifactNotFound)?;
                    p.release = Uuid::parse_str(&release).map_err(|_| Error::Storage)?;
                }
            }
            sqlx::query(
                "INSERT INTO authoring_operations(site_id,id,request,prepared) VALUES(?,?,?,?)",
            )
            .bind(site.to_string())
            .bind(input.id.to_string())
            .bind(&request)
            .bind(serde_json::to_string(&p).map_err(|_| Error::Storage)?)
            .execute(&self.0.pool)
            .await?;
            p
        };
        if let Some(files) = &prepared.files {
            let mut sources = prepared.migration_files.clone();
            sources.push(upload("frontend/index.html", &files.frontend));
            sources.push(upload("backend.ts", &files.backend));
            self.upload_revision(
                site,
                RevisionUpload {
                    id: prepared.revision,
                    files: sources,
                },
            )
            .await?;
            self.upload_release(
                site,
                ReleaseUpload {
                    id: prepared.release,
                    manifest: ReleaseManifest {
                        source_revision_id: prepared.revision,
                        frontend_entrypoint: "public/index.html".into(),
                        backend_entrypoint: "backend.js".into(),
                        sdk_version: "1".into(),
                        schema: prepared.schema.clone(),
                        migrations: prepared.migrations.clone(),
                    },
                    files: vec![
                        upload("public/index.html", &files.frontend),
                        upload("backend.js", &files.backend),
                    ],
                },
            )
            .await?;
        }
        let mut response =
            json!({"id":input.id,"siteId":site,"status":"succeeded","releaseId":prepared.release});
        if let Action::Snapshot { name } = &input.action {
            sqlx::query("INSERT INTO site_snapshots(site_id,id,name,release_id) VALUES(?,?,?,?) ON CONFLICT DO NOTHING").bind(site.to_string()).bind(input.id.to_string()).bind(name).bind(prepared.release.to_string()).execute(&self.0.pool).await?;
        } else {
            match self
                .activate_release(
                    site,
                    format!("author:{}", input.id),
                    Activation {
                        release_id: prepared.release,
                        expected_generation: prepared.generation,
                    },
                )
                .await
            {
                Ok(_) => {}
                Err(Error::Conflict(code, message)) => {
                    response = json!({"id":input.id,"siteId":site,"status":"conflict","error":{"code":code,"message":message}});
                }
                Err(error) => return Err(error),
            }
        }
        sqlx::query("UPDATE authoring_operations SET response=? WHERE site_id=? AND id=?")
            .bind(response.to_string())
            .bind(site.to_string())
            .bind(input.id.to_string())
            .execute(&self.0.pool)
            .await?;
        Ok(response)
    }
}
async fn validate_backend(code: &str, executable: &Path) -> Result<()> {
    let mut child = tool_code_mode::sandbox::command(executable, "--validate-backend")?.spawn()?;
    let mut stdin = child.stdin.take().ok_or(Error::Storage)?;
    let bytes = serde_json::to_vec(code).map_err(|_| Error::Storage)?;
    let work = async {
        stdin.write_all(&bytes).await?;
        stdin.shutdown().await?;
        drop(stdin);
        child.wait().await
    };
    match tokio::time::timeout(Duration::from_secs(3), work).await {
        Ok(Ok(status)) if status.success() => Ok(()),
        _ => {
            let _ = child.kill().await;
            let _ = child.wait().await;
            Err(Error::Invalid(
                "Backend validation failed: use a self-contained JavaScript module exporting a default handler.",
            ))
        }
    }
}
/// Validation runs only module initialization, without database/Platform bindings.
pub fn validate_guest() -> std::result::Result<(), Box<dyn std::error::Error>> {
    let mut bytes = vec![];
    std::io::stdin().take(384 * 1024).read_to_end(&mut bytes)?;
    let code: String = serde_json::from_slice(&bytes)?;
    let runtime = rquickjs::Runtime::new()?;
    runtime.set_memory_limit(64 * 1024 * 1024);
    runtime.set_max_stack_size(512 * 1024);
    let deadline = std::time::Instant::now() + Duration::from_secs(2);
    runtime.set_interrupt_handler(Some(Box::new(move || std::time::Instant::now() > deadline)));
    rquickjs::Context::full(&runtime)?.with(|ctx| -> rquickjs::Result<()> {
        let (module, evaluated) = rquickjs::Module::declare(ctx, "backend.js", code)?.eval()?;
        evaluated.finish::<()>()?;
        let _: rquickjs::Function = module.get("default")?;
        Ok(())
    })?;
    Ok(())
}
