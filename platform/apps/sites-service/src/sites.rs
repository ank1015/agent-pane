use std::{path::PathBuf, sync::Arc, time::Duration};

use serde::{Deserialize, Serialize};
use sqlx::{SqlitePool, sqlite::SqlitePoolOptions};
use tokio::sync::{Mutex, Notify, Semaphore, watch};
use uuid::Uuid;

use crate::{
    Error, Result,
    storage::{Storage, options},
};

#[derive(Clone, Copy, Debug, Deserialize, Serialize, sqlx::Type, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
#[sqlx(type_name = "TEXT", rename_all = "snake_case")]
pub enum Status {
    Provisioning,
    Ready,
    Suspended,
    Failed,
}

#[derive(Clone, Copy, Debug, Deserialize, Serialize, sqlx::Type, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
#[sqlx(type_name = "TEXT", rename_all = "snake_case")]
pub enum DesiredStatus {
    Ready,
    Suspended,
}

#[derive(Clone, Debug, Serialize, Deserialize, sqlx::FromRow)]
pub struct Site {
    pub id: String,
    pub project_id: String,
    pub status: Status,
    pub desired_status: DesiredStatus,
    pub error_code: Option<String>,
    pub created_at: String,
    pub updated_at: String,
    pub active_release_id: Option<String>,
    pub release_generation: i64,
}

#[derive(Clone)]
pub struct SiteService(pub(crate) Arc<Inner>);

pub(crate) struct Inner {
    pub pool: SqlitePool,
    pub platform: Option<crate::platform::PlatformClient>,
    // Serializes local lifecycle/reconciliation operations, not HTTP reads.
    pub mutations: Mutex<()>,
    pub authoring: Mutex<()>,
    pub publications: Arc<Semaphore>,
    changed: Notify,
    pub storage: Storage,
    pub executions: Arc<Semaphore>,
    pub gates: std::sync::Mutex<std::collections::HashMap<Uuid, std::sync::Weak<Mutex<()>>>>,
}

impl SiteService {
    pub async fn open(data_dir: PathBuf) -> Result<Self> {
        Self::open_with_platform(data_dir, None).await
    }

    pub async fn open_with_platform(
        data_dir: PathBuf,
        platform: Option<crate::platform::PlatformClient>,
    ) -> Result<Self> {
        let storage = Storage::open(data_dir).await?;
        let pool = SqlitePoolOptions::new()
            .max_connections(1)
            .acquire_timeout(Duration::from_secs(5))
            .connect_with(options(&storage.root.join("service.sqlite")))
            .await?;
        sqlx::migrate!("./migrations").run(&pool).await?;
        let service = Self(Arc::new(Inner {
            pool,
            platform,
            mutations: Mutex::new(()),
            authoring: Mutex::new(()),
            publications: Arc::new(Semaphore::new(2)),
            changed: Notify::new(),
            storage,
            executions: Arc::new(Semaphore::new(8)),
            gates: std::sync::Mutex::new(std::collections::HashMap::new()),
        }));
        // Validate initialized storage without recreating missing files; complete
        // any durable provisioning intents left by a killed process.
        service.reconcile_all().await?;
        service.recover_bundles().await?;
        sqlx::query("UPDATE invocations SET status='interrupted',error_code='SERVICE_RESTARTED',finished_at=strftime('%Y-%m-%dT%H:%M:%fZ','now') WHERE status='running'").execute(&service.0.pool).await?;
        Ok(service)
    }

    pub(crate) async fn site_gate(&self, site: Uuid) -> Result<tokio::sync::OwnedMutexGuard<()>> {
        let gate = {
            let mut gates = self.0.gates.lock().map_err(|_| Error::Storage)?;
            gates.retain(|_, g| g.strong_count() > 0);
            let gate = gates
                .get(&site)
                .and_then(std::sync::Weak::upgrade)
                .unwrap_or_else(|| Arc::new(Mutex::new(())));
            gates.insert(site, Arc::downgrade(&gate));
            gate
        };
        tokio::time::timeout(Duration::from_secs(5), gate.lock_owned())
            .await
            .map_err(|_| Error::Capacity)
    }

    pub async fn close(&self) {
        // Wait for detached invocations before closing their metadata store.
        let _drain = self.0.executions.acquire_many(8).await;
        self.0.pool.close().await;
    }

    pub async fn ready(&self) -> Result<()> {
        self.0.storage.check().await?;
        let changed = sqlx::query("UPDATE storage_health SET value=1-value WHERE id=1")
            .execute(&self.0.pool)
            .await?
            .rows_affected();
        if changed != 1 {
            return Err(Error::Storage);
        }
        Ok(())
    }

    pub async fn get(&self, id: Uuid) -> Result<Site> {
        sqlx::query_as("SELECT id,project_id,status,desired_status,error_code,created_at,updated_at,active_release_id,release_generation FROM sites WHERE id=?")
            .bind(id.to_string()).fetch_optional(&self.0.pool).await?.ok_or(Error::NotFound)
    }

    /// PUT is keyed by the immutable site/project binding and returns current state.
    /// Replays never reset data or undo suspension, and need no separate receipt key.
    pub async fn provision(&self, id: Uuid, project: Uuid) -> Result<Site> {
        let _guard = self.0.mutations.lock().await;
        sqlx::query("INSERT INTO sites (id,project_id) VALUES (?,?) ON CONFLICT(id) DO NOTHING")
            .bind(id.to_string())
            .bind(project.to_string())
            .execute(&self.0.pool)
            .await?;
        let site = self.get(id).await?;
        if site.project_id != project.to_string() {
            return Err(Error::ProjectConflict);
        }
        self.0.changed.notify_one();
        Ok(site)
    }

    /// Only desired lifecycle state is caller-controlled. This cannot claim that
    /// an uninitialized database is ready or recreate an initialized database.
    pub async fn set_status(&self, id: Uuid, status: DesiredStatus) -> Result<Site> {
        let _site = self.site_gate(id).await?;
        let _guard = self.0.mutations.lock().await;
        self.get(id).await?;
        sqlx::query("UPDATE sites SET desired_status=?, updated_at=strftime('%Y-%m-%dT%H:%M:%fZ','now') WHERE id=? AND desired_status<>?")
            .bind(status).bind(id.to_string()).bind(status).execute(&self.0.pool).await?;
        self.reconcile_site(id).await?;
        self.get(id).await
    }

    /// Periodic reconciliation retries provisioning errors and detects storage loss.
    /// Keyset pages bound memory; no database transaction spans filesystem work.
    pub async fn reconcile_all(&self) -> Result<()> {
        let mut after = String::new();
        loop {
            let ids: Vec<String> =
                sqlx::query_scalar("SELECT id FROM sites WHERE id>? ORDER BY id LIMIT 100")
                    .bind(&after)
                    .fetch_all(&self.0.pool)
                    .await?;
            if ids.is_empty() {
                return Ok(());
            }
            for id in ids {
                let parsed = Uuid::parse_str(&id).map_err(|_| Error::Storage)?;
                let _guard = self.0.mutations.lock().await;
                self.reconcile_site(parsed).await?;
                after = id;
            }
        }
    }

    pub async fn run_reconciler(&self, mut shutdown: watch::Receiver<bool>) {
        let mut interval = tokio::time::interval(Duration::from_secs(5));
        interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
        loop {
            if *shutdown.borrow() {
                return;
            }
            tokio::select! {
                _ = shutdown.changed() => return,
                _ = interval.tick() => {},
                _ = self.0.changed.notified() => {},
            }
            // Finish a reconciliation pass on shutdown instead of dropping SQLite work.
            if let Err(error) = self.reconcile_all().await {
                tracing::error!(error = %error, "site reconciliation failed");
            }
        }
    }

    async fn reconcile_site(&self, id: Uuid) -> Result<()> {
        let site = self.get(id).await?;
        let initialized: bool =
            sqlx::query_scalar("SELECT storage_initialized FROM sites WHERE id=?")
                .bind(id.to_string())
                .fetch_one(&self.0.pool)
                .await?;
        let project = Uuid::parse_str(&site.project_id).map_err(|_| Error::Storage)?;
        match self
            .0
            .storage
            .site_database(id, project, !initialized)
            .await
        {
            Ok(()) => {
                sqlx::query("UPDATE sites SET storage_initialized=1,status=desired_status,error_code=NULL,updated_at=strftime('%Y-%m-%dT%H:%M:%fZ','now') WHERE id=? AND (storage_initialized=0 OR status<>desired_status OR error_code IS NOT NULL)")
                    .bind(id.to_string()).execute(&self.0.pool).await?;
            }
            Err(error) => {
                let code = if initialized {
                    "SITE_STORAGE_UNAVAILABLE"
                } else {
                    "SITE_PROVISIONING_FAILED"
                };
                if site.error_code.as_deref() != Some(code) {
                    tracing::warn!(site_id = %id, error = %error, "site storage reconciliation failed");
                }
                sqlx::query("UPDATE sites SET status='failed',error_code=?,updated_at=strftime('%Y-%m-%dT%H:%M:%fZ','now') WHERE id=? AND (status<>'failed' OR error_code IS NOT ?)")
                    .bind(code).bind(id.to_string()).bind(code).execute(&self.0.pool).await?;
            }
        }
        Ok(())
    }
}
