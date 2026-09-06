//! Immutable bundle metadata and publication. Application databases are not touched.
use std::collections::{BTreeMap, BTreeSet};

use base64::{Engine, engine::general_purpose::STANDARD};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use uuid::Uuid;

use crate::{Error, Result, Site, SiteService, Status, bundle_files};

pub const UPLOAD_BODY_LIMIT: usize = 24 * 1024 * 1024;
pub const MAX_FILE_BYTES: usize = 4 * 1024 * 1024;
pub const MAX_BUNDLE_BYTES: usize = 16 * 1024 * 1024;
pub const MAX_FILES: usize = 256;

#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum Kind {
    Revision,
    Release,
}
impl Kind {
    pub(crate) fn name(self) -> &'static str {
        match self {
            Self::Revision => "revision",
            Self::Release => "release",
        }
    }
    pub(crate) fn directory(self) -> &'static str {
        match self {
            Self::Revision => "revisions",
            Self::Release => "releases",
        }
    }
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct FileUpload {
    pub path: String,
    pub content_base64: String,
    pub sha256: String,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct RevisionUpload {
    pub id: Uuid,
    pub files: Vec<FileUpload>,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct ReleaseManifest {
    pub source_revision_id: Uuid,
    pub frontend_entrypoint: String,
    pub backend_entrypoint: String,
    pub sdk_version: String,
    #[serde(default, skip_serializing_if = "SchemaRange::is_zero")]
    pub schema: SchemaRange,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub migrations: Vec<MigrationFile>,
}

#[derive(Clone, Debug, Default, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct SchemaRange {
    pub min: u32,
    pub max: u32,
}
impl SchemaRange {
    fn is_zero(&self) -> bool {
        self.min == 0 && self.max == 0
    }
}
#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct MigrationFile {
    pub version: u32,
    pub path: String,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ReleaseUpload {
    pub id: Uuid,
    pub manifest: ReleaseManifest,
    pub files: Vec<FileUpload>,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
pub struct FileInfo {
    pub size: usize,
    pub sha256: String,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
pub struct Descriptor {
    pub format_version: u32,
    pub site_id: Uuid,
    pub id: Uuid,
    pub kind: Kind,
    pub manifest: Option<ReleaseManifest>,
    pub files: BTreeMap<String, FileInfo>,
}

#[derive(Debug, Deserialize, Serialize)]
pub struct Bundle {
    #[serde(flatten)]
    pub descriptor: Descriptor,
    pub status: String,
    pub error_code: Option<String>,
    pub created_at: String,
}

#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Activation {
    pub release_id: Uuid,
    pub expected_generation: i64,
}

#[derive(sqlx::FromRow)]
pub(crate) struct BundleRow {
    pub descriptor: String,
    pub fingerprint: String,
    pub status: String,
    pub error_code: Option<String>,
    pub created_at: String,
}
impl BundleRow {
    pub fn bundle(self) -> Result<Bundle> {
        Ok(Bundle {
            descriptor: serde_json::from_str(&self.descriptor).map_err(|_| Error::Storage)?,
            status: self.status,
            error_code: self.error_code,
            created_at: self.created_at,
        })
    }
}

pub(crate) fn digest(bytes: &[u8]) -> String {
    format!("{:x}", Sha256::digest(bytes))
}

/// Restrict to portable, unambiguous URL/file paths. No decoding, backslashes,
/// hidden components, platform device names, or case-insensitive collisions.
pub(crate) fn validate_path(path: &str) -> Result<()> {
    if path.is_empty() || path.len() > 512 {
        return Err(Error::Invalid("Invalid bundle path."));
    }
    for part in path.split('/') {
        if part.is_empty()
            || part.len() > 128
            || part.starts_with('.')
            || part.ends_with('.')
            || !part
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b"._-".contains(&b))
        {
            return Err(Error::Invalid(
                "Bundle paths must be portable relative paths without hidden or dot components.",
            ));
        }
        let stem = part.split('.').next().unwrap_or("").to_ascii_lowercase();
        if ["con", "prn", "aux", "nul"].contains(&stem.as_str())
            || (stem.len() == 4
                && (stem.starts_with("com") || stem.starts_with("lpt"))
                && stem.as_bytes()[3].is_ascii_digit())
        {
            return Err(Error::Invalid("Reserved device names are not supported."));
        }
    }
    Ok(())
}

fn prepare(
    site: Uuid,
    id: Uuid,
    kind: Kind,
    manifest: Option<ReleaseManifest>,
    uploads: Vec<FileUpload>,
) -> Result<(Descriptor, BTreeMap<String, Vec<u8>>)> {
    if uploads.is_empty() || uploads.len() > MAX_FILES {
        return Err(Error::Invalid("Bundles require 1–256 files."));
    }
    let mut files = BTreeMap::new();
    let mut infos = BTreeMap::new();
    let mut names = BTreeSet::new();
    let mut component_names = BTreeMap::new();
    let mut total = 0;
    for file in uploads {
        validate_path(&file.path)?;
        let mut prefix = String::new();
        for part in file.path.split('/') {
            if !prefix.is_empty() {
                prefix.push('/');
            }
            prefix.push_str(part);
            if let Some(previous) =
                component_names.insert(prefix.to_ascii_lowercase(), prefix.clone())
            {
                if previous != prefix {
                    return Err(Error::Invalid(
                        "Directory components must use consistent casing.",
                    ));
                }
            }
        }
        if !names.insert(file.path.to_ascii_lowercase()) {
            return Err(Error::Invalid("Duplicate or case-colliding bundle path."));
        }
        if file.content_base64.len() > MAX_FILE_BYTES.div_ceil(3) * 4 {
            return Err(Error::Invalid("File exceeds 4 MiB."));
        }
        let bytes = STANDARD
            .decode(&file.content_base64)
            .map_err(|_| Error::Invalid("Invalid file base64."))?;
        total += bytes.len();
        if bytes.len() > MAX_FILE_BYTES || total > MAX_BUNDLE_BYTES {
            return Err(Error::Invalid("File or bundle size limit exceeded."));
        }
        if file.sha256.len() != 64
            || !file
                .sha256
                .bytes()
                .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
            || digest(&bytes) != file.sha256
        {
            return Err(Error::Invalid(
                "File SHA-256 mismatch; use lowercase hex checksums.",
            ));
        }
        infos.insert(
            file.path.clone(),
            FileInfo {
                size: bytes.len(),
                sha256: file.sha256,
            },
        );
        files.insert(file.path, bytes);
    }
    for name in &names {
        let mut prefix = String::new();
        let parts: Vec<_> = name.split('/').collect();
        for part in &parts[..parts.len() - 1] {
            if !prefix.is_empty() {
                prefix.push('/');
            }
            prefix.push_str(part);
            if names.contains(&prefix) {
                return Err(Error::Invalid("File/directory path collision."));
            }
        }
    }
    match (&kind, &manifest) {
        (Kind::Revision, None) => {
            if !files.contains_key("backend.ts")
                || !files.keys().any(|p| p.starts_with("frontend/"))
            {
                return Err(Error::Invalid(
                    "Source requires backend.ts and frontend files.",
                ));
            }
        }
        (Kind::Release, Some(m)) => {
            validate_path(&m.frontend_entrypoint)?;
            if m.schema.min > m.schema.max || m.schema.max > 10_000 || m.migrations.len() > 100 {
                return Err(Error::Invalid(
                    "Invalid schema compatibility range or migration count.",
                ));
            }
            for (i, migration) in m.migrations.iter().enumerate() {
                validate_path(&migration.path)?;
                if migration.version != (i + 1) as u32
                    || !migration.path.starts_with("migrations/")
                    || !migration.path.ends_with(".sql")
                {
                    return Err(Error::Invalid(
                        "Migrations must be consecutive from version 1 and reference migrations/*.sql in the source revision.",
                    ));
                }
            }
            if m.schema.min > m.migrations.len() as u32
                || (m.migrations.len() as u32) > m.schema.max
            {
                return Err(Error::Invalid(
                    "Schema range must include the declared migration version.",
                ));
            }
            if m.sdk_version != "1" {
                return Err(Error::Invalid("Only site SDK version 1 is supported."));
            }
            if m.backend_entrypoint != "backend.js"
                || !m.frontend_entrypoint.starts_with("public/")
                || !m.frontend_entrypoint.ends_with(".html")
            {
                return Err(Error::Invalid(
                    "Release entrypoints must be public/*.html and backend.js.",
                ));
            }
            for entry in [&m.frontend_entrypoint, &m.backend_entrypoint] {
                let bytes = files
                    .get(entry)
                    .ok_or(Error::Invalid("Release entrypoint missing."))?;
                if bytes.is_empty() || std::str::from_utf8(bytes).is_err() {
                    return Err(Error::Invalid("Entrypoints must be nonempty UTF-8 files."));
                }
            }
            if files
                .keys()
                .any(|p| p != "backend.js" && !p.starts_with("public/"))
            {
                return Err(Error::Invalid(
                    "Release files must be under public/ or be backend.js.",
                ));
            }
        }
        _ => return Err(Error::Invalid("Invalid bundle manifest.")),
    }
    Ok((
        Descriptor {
            format_version: 1,
            site_id: site,
            id,
            kind,
            manifest,
            files: infos,
        },
        files,
    ))
}

impl SiteService {
    pub(crate) async fn require_ready(&self, site: Uuid) -> Result<()> {
        if self.get(site).await?.status != Status::Ready {
            return Err(Error::Conflict(
                "SITE_NOT_READY",
                "Site must be ready and not suspended.",
            ));
        }
        Ok(())
    }

    pub async fn upload_revision(&self, site: Uuid, input: RevisionUpload) -> Result<Bundle> {
        let (descriptor, files) = prepare(site, input.id, Kind::Revision, None, input.files)?;
        self.upload(descriptor, files).await
    }

    pub async fn upload_release(&self, site: Uuid, input: ReleaseUpload) -> Result<Bundle> {
        let (descriptor, files) = prepare(
            site,
            input.id,
            Kind::Release,
            Some(input.manifest),
            input.files,
        )?;
        self.upload(descriptor, files).await
    }

    async fn upload(
        &self,
        descriptor: Descriptor,
        files: BTreeMap<String, Vec<u8>>,
    ) -> Result<Bundle> {
        // Retain admission through detached publication, including after the HTTP
        // request is gone. Otherwise disconnects could bypass the upload bound.
        let permit = self
            .0
            .publications
            .clone()
            .try_acquire_owned()
            .map_err(|_| Error::Capacity)?;
        let service = self.clone();
        // Keep the publication owner alive if the HTTP caller disconnects. A process
        // crash still relies on staging recovery or an identical upload retry.
        tokio::spawn(async move {
            let _permit = permit;
            let _guard = service.0.mutations.lock().await;
            service.require_ready(descriptor.site_id).await?;
            if let Some(manifest) = &descriptor.manifest {
                let source = service.bundle(descriptor.site_id, Kind::Revision, manifest.source_revision_id).await?;
                if source.status != "ready" { return Err(Error::Conflict("SOURCE_NOT_READY", "Source revision is not ready.")); }
                for migration in &manifest.migrations {
                    if !source.descriptor.files.contains_key(&migration.path) {
                        return Err(Error::Invalid("Migration file is absent from source revision."));
                    }
                }
            }
            let encoded = serde_json::to_string(&descriptor).map_err(|_| Error::Storage)?;
            let fingerprint = digest(encoded.as_bytes());
            sqlx::query("INSERT INTO bundles(site_id,kind,id,fingerprint,descriptor,status) VALUES(?,?,?,?,?,'staging') ON CONFLICT(site_id,kind,id) DO NOTHING")
                .bind(descriptor.site_id.to_string()).bind(descriptor.kind.name()).bind(descriptor.id.to_string())
                .bind(&fingerprint).bind(&encoded).execute(&service.0.pool).await?;
            let row = service.bundle_row(descriptor.site_id, descriptor.kind, descriptor.id).await?;
            if row.fingerprint != fingerprint { return Err(Error::Conflict("BUNDLE_IMMUTABLE", "Bundle ID already identifies different content.")); }
            let root = service.0.storage.root.clone();
            let disk_descriptor = descriptor.clone();
            let already_ready = row.status == "ready";
            if !already_ready {
                // A retry has the same durable intent as an initial upload. If it
                // crashes, startup must inspect its staging/final directory too.
                sqlx::query("UPDATE bundles SET status='staging',error_code=NULL WHERE site_id=? AND kind=? AND id=?")
                    .bind(descriptor.site_id.to_string()).bind(descriptor.kind.name()).bind(descriptor.id.to_string())
                    .execute(&service.0.pool).await?;
            }
            let outcome = tokio::task::spawn_blocking(move || {
                if already_ready { bundle_files::verify_final(&root, &disk_descriptor) }
                else { bundle_files::publish(&root, &disk_descriptor, &files) }
            }).await?;
            service.finish_bundle(&descriptor, outcome.is_ok()).await?;
            outcome?;
            service.bundle(descriptor.site_id, descriptor.kind, descriptor.id).await
        }).await?
    }

    pub async fn bundle(&self, site: Uuid, kind: Kind, id: Uuid) -> Result<Bundle> {
        self.get(site).await?;
        self.bundle_row(site, kind, id).await?.bundle()
    }

    pub(crate) async fn bundle_row(&self, site: Uuid, kind: Kind, id: Uuid) -> Result<BundleRow> {
        sqlx::query_as("SELECT descriptor,fingerprint,status,error_code,created_at FROM bundles WHERE site_id=? AND kind=? AND id=?")
            .bind(site.to_string()).bind(kind.name()).bind(id.to_string()).fetch_optional(&self.0.pool).await?.ok_or(Error::ArtifactNotFound)
    }

    pub async fn list_releases(
        &self,
        site: Uuid,
        after: Option<Uuid>,
        limit: u32,
    ) -> Result<Vec<Bundle>> {
        self.get(site).await?;
        if !(1..=100).contains(&limit) {
            return Err(Error::Invalid("Limit must be between 1 and 100."));
        }
        let rows: Vec<BundleRow> = sqlx::query_as("SELECT descriptor,fingerprint,status,error_code,created_at FROM bundles WHERE site_id=? AND kind='release' AND id>? ORDER BY id LIMIT ?")
            .bind(site.to_string()).bind(after.map(|id| id.to_string()).unwrap_or_default()).bind(limit).fetch_all(&self.0.pool).await?;
        rows.into_iter().map(BundleRow::bundle).collect()
    }

    pub async fn read_bundle_file(
        &self,
        site: Uuid,
        kind: Kind,
        id: Uuid,
        path: String,
    ) -> Result<Vec<u8>> {
        validate_path(&path)?;
        let bundle = self.bundle(site, kind, id).await?;
        if bundle.status != "ready" {
            return Err(Error::Conflict("BUNDLE_NOT_READY", "Bundle is not ready."));
        }
        let root = self.0.storage.root.clone();
        tokio::task::spawn_blocking(move || {
            bundle_files::read_file(&root, &bundle.descriptor, &path)
        })
        .await?
    }

    pub async fn activate_release(
        &self,
        site: Uuid,
        key: String,
        input: Activation,
    ) -> Result<Site> {
        if key.is_empty()
            || key.len() > 256
            || !key.bytes().all(|b| b.is_ascii_graphic())
            || input.expected_generation < 0
        {
            return Err(Error::Invalid(
                "Provide an Idempotency-Key and nonnegative expected_generation.",
            ));
        }
        let _site = self.site_gate(site).await?;
        let _guard = self.0.mutations.lock().await;
        self.get(site).await?;
        let fingerprint = digest(&serde_json::to_vec(&input).map_err(|_| Error::Storage)?);
        let receipt: Option<(String,String)> = sqlx::query_as("SELECT fingerprint,response FROM activation_receipts WHERE site_id=? AND operation_key=?")
            .bind(site.to_string()).bind(&key).fetch_optional(&self.0.pool).await?;
        if let Some((saved, response)) = receipt {
            if saved != fingerprint {
                return Err(Error::Conflict(
                    "IDEMPOTENCY_CONFLICT",
                    "Operation key identifies a different activation.",
                ));
            }
            return serde_json::from_str(&response).map_err(|_| Error::Storage);
        }
        self.require_ready(site).await?;
        let release = self.bundle(site, Kind::Release, input.release_id).await?;
        if release.status != "ready" {
            return Err(Error::Conflict("BUNDLE_NOT_READY", "Release is not ready."));
        }
        self.check_release_schema(site, &release).await?;
        let root = self.0.storage.root.clone();
        tokio::task::spawn_blocking(move || bundle_files::verify_final(&root, &release.descriptor))
            .await??;
        let mut tx = self.0.pool.begin().await?;
        let changed = sqlx::query("UPDATE sites SET active_release_id=?,release_generation=release_generation+1,updated_at=strftime('%Y-%m-%dT%H:%M:%fZ','now') WHERE id=? AND release_generation=? AND release_generation<9223372036854775807")
            .bind(input.release_id.to_string()).bind(site.to_string()).bind(input.expected_generation).execute(&mut *tx).await?.rows_affected();
        if changed != 1 {
            return Err(Error::Conflict(
                "RELEASE_GENERATION_CONFLICT",
                "Active release changed. Reload before activating.",
            ));
        }
        let result: Site = sqlx::query_as("SELECT id,project_id,status,desired_status,error_code,created_at,updated_at,active_release_id,release_generation FROM sites WHERE id=?")
            .bind(site.to_string()).fetch_one(&mut *tx).await?;
        sqlx::query("INSERT INTO activation_receipts VALUES(?,?,?,?)")
            .bind(site.to_string())
            .bind(key)
            .bind(fingerprint)
            .bind(serde_json::to_string(&result).map_err(|_| Error::Storage)?)
            .execute(&mut *tx)
            .await?;
        tx.commit().await?;
        Ok(result)
    }

    pub(crate) async fn finish_bundle(&self, descriptor: &Descriptor, success: bool) -> Result<()> {
        sqlx::query("UPDATE bundles SET status=?,error_code=? WHERE site_id=? AND kind=? AND id=?")
            .bind(if success { "ready" } else { "failed" })
            .bind(if success {
                None
            } else {
                Some("BUNDLE_UPLOAD_INCOMPLETE")
            })
            .bind(descriptor.site_id.to_string())
            .bind(descriptor.kind.name())
            .bind(descriptor.id.to_string())
            .execute(&self.0.pool)
            .await?;
        Ok(())
    }

    pub async fn recover_bundles(&self) -> Result<()> {
        let _guard = self.0.mutations.lock().await;
        // Cursor uses the composite primary key; each page is bounded.
        let mut cursor = (String::new(), String::new(), String::new());
        loop {
            let rows: Vec<BundleRow> = sqlx::query_as("SELECT descriptor,fingerprint,status,error_code,created_at FROM bundles WHERE status='staging' AND (site_id,kind,id)>(?,?,?) ORDER BY site_id,kind,id LIMIT 100")
                .bind(&cursor.0).bind(&cursor.1).bind(&cursor.2).fetch_all(&self.0.pool).await?;
            if rows.is_empty() {
                return Ok(());
            }
            for row in rows {
                let descriptor = row.bundle()?.descriptor;
                cursor = (
                    descriptor.site_id.to_string(),
                    descriptor.kind.name().into(),
                    descriptor.id.to_string(),
                );
                let root = self.0.storage.root.clone();
                let copy = descriptor.clone();
                let success =
                    tokio::task::spawn_blocking(move || bundle_files::recover(&root, &copy))
                        .await?
                        .is_ok();
                self.finish_bundle(&descriptor, success).await?;
            }
        }
    }
}
