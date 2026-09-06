use std::{
    fs::{self, File, OpenOptions},
    path::{Path, PathBuf},
    time::Duration,
};

use sqlx::{
    Connection, SqliteConnection,
    sqlite::{SqliteConnectOptions, SqliteJournalMode, SqliteSynchronous},
};
use uuid::Uuid;

use crate::{Error, Result};

pub(crate) struct Storage {
    pub root: PathBuf,
    // Never unlink this file: doing so would allow another owner to lock a new inode.
    _lock: File,
}

impl Storage {
    pub async fn open(root: PathBuf) -> Result<Self> {
        tokio::task::spawn_blocking(move || {
            if !root.is_absolute() {
                return Err(Error::Config("data directory must be absolute"));
            }
            directory(&root)?;
            let root = root.canonicalize()?;
            let lock_path = root.join(".owner.lock");
            regular_file(&lock_path, true)?;
            let lock = OpenOptions::new().read(true).write(true).open(lock_path)?;
            fs2::FileExt::try_lock_exclusive(&lock).map_err(|_| Error::Locked)?;
            let sites = root.join("sites");
            directory(&sites)?;
            let database = root.join("service.sqlite");
            // Never silently replace lost metadata beside existing application data.
            let empty = match fs::symlink_metadata(&database) {
                Ok(meta) => meta.len() == 0,
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => true,
                Err(e) => return Err(e.into()),
            };
            if empty && fs::read_dir(&sites)?.next().is_some() {
                return Err(Error::Storage);
            }
            sqlite_file(&database, true)?;
            Ok(Self { root, _lock: lock })
        })
        .await?
    }

    pub async fn check(&self) -> Result<()> {
        let root = self.root.clone();
        tokio::task::spawn_blocking(move || {
            existing_directory(&root)?;
            existing_directory(&root.join("sites"))?;
            sqlite_file(&root.join("service.sqlite"), false)
        })
        .await?
    }

    pub async fn site_database(&self, site: Uuid, project: Uuid, create: bool) -> Result<()> {
        let root = self.root.clone();
        let path = tokio::task::spawn_blocking(move || {
            existing_directory(&root)?;
            existing_directory(&root.join("sites"))?;
            let site_dir = root.join("sites").join(site.to_string());
            let data_dir = site_dir.join("data");
            if create {
                directory(&site_dir)?;
                directory(&data_dir)?;
            } else {
                existing_directory(&site_dir)?;
                existing_directory(&data_dir)?;
            }
            let path = data_dir.join("site.sqlite");
            sqlite_file(&path, create)?;
            Ok::<_, Error>(path)
        })
        .await??;
        let mut connection = SqliteConnection::connect_with(&options(&path)).await?;
        let result = verify_identity(&mut connection, site, project, create).await;
        // Finish SQLite work before allowing another reconciliation of this file.
        let closed = connection.close().await;
        result?;
        closed?;
        // Persist directory entries before marking storage initialized in metadata.
        tokio::task::spawn_blocking(move || sync_directory(path.parent().ok_or(Error::Storage)?))
            .await??;
        Ok(())
    }
}

pub(crate) fn options(path: &Path) -> SqliteConnectOptions {
    SqliteConnectOptions::new()
        .filename(path)
        .create_if_missing(false)
        .journal_mode(SqliteJournalMode::Wal)
        .synchronous(SqliteSynchronous::Full)
        .foreign_keys(true)
        .busy_timeout(Duration::from_secs(5))
}

async fn verify_identity(
    connection: &mut SqliteConnection,
    site: Uuid,
    project: Uuid,
    create: bool,
) -> Result<()> {
    let mut tx = connection.begin().await?;
    let exists: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM sqlite_schema WHERE type='table' AND name='__sites_identity'",
    )
    .fetch_one(&mut *tx)
    .await?;
    if exists == 0 {
        let tables: i64 =
            sqlx::query_scalar("SELECT count(*) FROM sqlite_schema WHERE name NOT LIKE 'sqlite_%'")
                .fetch_one(&mut *tx)
                .await?;
        if !create || tables != 0 {
            return Err(Error::Storage);
        }
        sqlx::query("CREATE TABLE __sites_identity (singleton INTEGER PRIMARY KEY CHECK(singleton=1), site_id TEXT NOT NULL, project_id TEXT NOT NULL)")
            .execute(&mut *tx).await?;
        sqlx::query("INSERT INTO __sites_identity VALUES (1, ?, ?)")
            .bind(site.to_string())
            .bind(project.to_string())
            .execute(&mut *tx)
            .await?;
    }
    let identity: Option<(String, String)> =
        sqlx::query_as("SELECT site_id, project_id FROM __sites_identity WHERE singleton=1")
            .fetch_optional(&mut *tx)
            .await?;
    if identity != Some((site.to_string(), project.to_string())) {
        return Err(Error::Storage);
    }
    tx.commit().await?;
    Ok(())
}

pub(crate) fn existing_directory(path: &Path) -> Result<()> {
    let meta = fs::symlink_metadata(path)?;
    if !meta.is_dir() || meta.file_type().is_symlink() {
        return Err(Error::Storage);
    }
    Ok(())
}

pub(crate) fn directory(path: &Path) -> Result<()> {
    match fs::symlink_metadata(path) {
        Ok(_) => existing_directory(path)?,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            let mut builder = fs::DirBuilder::new();
            builder.recursive(true);
            #[cfg(unix)]
            {
                use std::os::unix::fs::DirBuilderExt;
                builder.mode(0o700);
            }
            builder.create(path)?;
            sync_directory(path)?;
            if let Some(parent) = path.parent() {
                sync_directory(parent)?;
            }
        }
        Err(e) => return Err(e.into()),
    }
    // This is a dedicated private volume, not a directory shared with other services.
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(path, fs::Permissions::from_mode(0o700))?;
    }
    Ok(())
}

fn regular_file(path: &Path, create: bool) -> Result<()> {
    match fs::symlink_metadata(path) {
        Ok(meta) if meta.is_file() && !meta.file_type().is_symlink() => {}
        Ok(_) => return Err(Error::Storage),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound && create => {
            let mut options = OpenOptions::new();
            options.write(true).create_new(true);
            #[cfg(unix)]
            {
                use std::os::unix::fs::OpenOptionsExt;
                options.mode(0o600);
            }
            options.open(path)?.sync_all()?;
            if let Some(parent) = path.parent() {
                sync_directory(parent)?;
            }
        }
        Err(e) => return Err(e.into()),
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(path, fs::Permissions::from_mode(0o600))?;
    }
    Ok(())
}

pub(crate) fn sqlite_file(path: &Path, create: bool) -> Result<()> {
    regular_file(path, create)?;
    for suffix in ["-wal", "-shm", "-journal"] {
        let mut sidecar = path.as_os_str().to_os_string();
        sidecar.push(suffix);
        let sidecar = PathBuf::from(sidecar);
        match fs::symlink_metadata(&sidecar) {
            Ok(_) => regular_file(&sidecar, false)?,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(e) => return Err(e.into()),
        }
    }
    Ok(())
}

pub(crate) fn sync_directory(path: &Path) -> Result<()> {
    #[cfg(unix)]
    File::open(path)?.sync_all()?;
    #[cfg(not(unix))]
    let _ = path;
    Ok(())
}
