//! Publication-time migrations and consistent immutable SQLite backups.
use crate::{Error, Result, SiteService, Status, bundles::Kind, database, storage};
use rusqlite::{
    Connection,
    hooks::{AuthContext, Authorization},
};
use serde_json::{Value, json};
use std::{
    path::PathBuf,
    time::{Duration, Instant},
};
use uuid::Uuid;

impl SiteService {
    pub async fn schema(&self, site: Uuid) -> Result<Value> {
        let _gate = self.site_gate(site).await?;
        let record = self.get(site).await?;
        let project = Uuid::parse_str(&record.project_id).map_err(|_| Error::Storage)?;
        let root = self.0.storage.root.clone();
        tokio::task::spawn_blocking(move || schema_info(&database::open(&root, site, project)?))
            .await?
    }
    pub async fn apply_migrations(&self, site: Uuid, release_id: Uuid) -> Result<Value> {
        let permit = self
            .0
            .executions
            .clone()
            .try_acquire_owned()
            .map_err(|_| Error::Capacity)?;
        let service = self.clone();
        tokio::spawn(async move {
            let _permit = permit;
            let _gate = service.site_gate(site).await?;
            let record = service.get(site).await?;
            if record.status != Status::Suspended { return Err(Error::Conflict("SUSPEND_FOR_MIGRATION", "Suspend the site before applying schema migrations.")); }
            let project = Uuid::parse_str(&record.project_id).map_err(|_| Error::Storage)?;
            let release = service.bundle(site,Kind::Release,release_id).await?;
            if release.status != "ready" { return Err(Error::Conflict("BUNDLE_NOT_READY", "Release is not ready.")); }
            let manifest = release.descriptor.manifest.ok_or(Error::Storage)?;
            let mut migrations = Vec::new();
            let mut total = 0;
            for file in manifest.migrations {
                let bytes = service.read_bundle_file(site,Kind::Revision,manifest.source_revision_id,file.path).await?;
                total += bytes.len();
                if bytes.len() > database::MAX_SQL || total > 1024 * 1024 { return Err(Error::Invalid("Migration SQL exceeds size limits.")); }
                let checksum = crate::bundles::digest(&bytes);
                let sql = String::from_utf8(bytes).map_err(|_| Error::Invalid("Migration SQL must be UTF-8."))?;
                migrations.push((file.version,checksum,sql));
            }
            let root = service.0.storage.root.clone();
            tokio::task::spawn_blocking(move || {
                let mut conn = database::open(&root,site,project)?;
                let deadline = Instant::now() + Duration::from_secs(10);
                conn.progress_handler(1000,Some(move || Instant::now() >= deadline));
                let tx = conn.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate).map_err(|_| Error::Storage)?;
                let mut current = database::version(&tx)?;
                for (version,checksum,sql) in migrations {
                    let old = tx.query_row("SELECT checksum FROM __sites_migrations WHERE version=?", [version], |r| r.get::<_,String>(0));
                    match old {
                        Ok(old) if old == checksum => continue,
                        Ok(_) => return Err(Error::Conflict("MIGRATION_CONFLICT", "An applied migration has different content.")),
                        Err(rusqlite::Error::QueryReturnedNoRows) => {},
                        Err(_) => return Err(Error::Storage),
                    }
                    if version != current + 1 { return Err(Error::Conflict("MIGRATION_ORDER", "Migration history is not consecutive.")); }
                    tx.authorizer(Some(|ctx: AuthContext<'_>| database::authorize(ctx,true)));
                    let result = tx.execute_batch(&sql);
                    tx.authorizer(None::<fn(AuthContext<'_>) -> Authorization>);
                    result.map_err(|_| Error::Conflict("MIGRATION_REJECTED", "Migration SQL failed or attempted a forbidden operation."))?;
                    // ALTER TABLE can rename into a reserved name; check catalog
                    // before committing as well as using the SQLite authorizer.
                    let invalid: i64 = tx.query_row("SELECT count(*) FROM sqlite_schema WHERE lower(name) GLOB '__sites_*' AND name NOT IN ('__sites_identity','__sites_migrations')", [], |r| r.get(0)).map_err(|_| Error::Storage)?;
                    if invalid != 0 { return Err(Error::Conflict("MIGRATION_REJECTED", "Migration uses reserved object names.")); }
                    tx.execute("INSERT INTO __sites_migrations(version,checksum) VALUES(?,?)",rusqlite::params![version,checksum]).map_err(|_| Error::Storage)?;
                    current = version;
                }
                if current < manifest.schema.min || current > manifest.schema.max {
                    return Err(Error::Conflict("SCHEMA_INCOMPATIBLE", "Migration would not produce a schema supported by this release."));
                }
                tx.commit().map_err(|_| Error::Storage)?;
                schema_info(&conn)
            }).await?
        }).await?
    }
    pub async fn backup(&self, site: Uuid, id: Uuid) -> Result<Value> {
        let permit = self
            .0
            .executions
            .clone()
            .try_acquire_owned()
            .map_err(|_| Error::Capacity)?;
        let service = self.clone();
        tokio::spawn(async move {
            let _permit = permit;
            let _gate = service.site_gate(site).await?;
            let record = service.get(site).await?;
            if !matches!(record.status, Status::Ready | Status::Suspended) {
                return Err(Error::Storage);
            }
            let project = Uuid::parse_str(&record.project_id).map_err(|_| Error::Storage)?;
            let root = service.0.storage.root.clone();
            tokio::task::spawn_blocking(move || {
                let conn = database::open(&root, site, project)?;
                let dir = root.join("sites").join(site.to_string()).join("backups");
                storage::directory(&dir)?;
                let target = dir.join(format!("{id}.sqlite"));
                let present = match std::fs::symlink_metadata(&target) {
                    Ok(_) => {
                        storage::sqlite_file(&target, false)?;
                        true
                    }
                    Err(e) if e.kind() == std::io::ErrorKind::NotFound => false,
                    Err(e) => return Err(e.into()),
                };
                if !present {
                    let partial = dir.join(format!("{id}.partial"));
                    storage::sqlite_file(&partial, true)?;
                    let mut dest = Connection::open(&partial).map_err(|_| Error::Storage)?;
                    let backup = rusqlite::backup::Backup::new(&conn, &mut dest)
                        .map_err(|_| Error::Storage)?;
                    let deadline = Instant::now() + Duration::from_secs(10);
                    loop {
                        if Instant::now() >= deadline {
                            return Err(Error::Capacity);
                        }
                        match backup.step(128).map_err(|_| Error::Storage)? {
                            rusqlite::backup::StepResult::Done => break,
                            rusqlite::backup::StepResult::More => {}
                            _ => return Err(Error::Capacity),
                        }
                    }
                    drop(backup);
                    dest.execute_batch("PRAGMA journal_mode=DELETE;")
                        .map_err(|_| Error::Storage)?;
                    dest.close().map_err(|_| Error::Storage)?;
                    std::fs::File::open(&partial)?.sync_all()?;
                    std::fs::rename(&partial, &target)?;
                    storage::sync_directory(&dir)?;
                }
                storage::sqlite_file(&target, false)?;
                let length = std::fs::metadata(&target)?.len();
                Ok(json!({"id":id,"site_id":site,"size":length}))
            })
            .await?
        })
        .await?
    }
    pub async fn backup_file(&self, site: Uuid, id: Uuid) -> Result<PathBuf> {
        self.get(site).await?;
        let root = self.0.storage.root.clone();
        tokio::task::spawn_blocking(move || {
            database::path(&root, site)?;
            let dir = root.join("sites").join(site.to_string()).join("backups");
            storage::existing_directory(&dir)?;
            let file = dir.join(format!("{id}.sqlite"));
            storage::sqlite_file(&file, false)?;
            Ok(file)
        })
        .await?
    }
}
fn schema_info(conn: &Connection) -> Result<Value> {
    let mut stmt = conn
        .prepare("SELECT version,checksum FROM __sites_migrations ORDER BY version")
        .map_err(|_| Error::Storage)?;
    let rows = stmt
        .query_map([], |r| {
            Ok(json!({"version":r.get::<_,u32>(0)?,"checksum":r.get::<_,String>(1)?}))
        })
        .map_err(|_| Error::Storage)?;
    let migrations = rows
        .collect::<std::result::Result<Vec<_>, _>>()
        .map_err(|_| Error::Storage)?;
    Ok(json!({"version":database::version(conn)?,"migrations":migrations}))
}
