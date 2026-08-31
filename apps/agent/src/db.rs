use std::time::Duration;

use sqlx::{
    PgPool,
    migrate::MigrateError,
    postgres::{PgListener, PgPoolOptions},
};
use tokio::{sync::broadcast, task::JoinHandle};
use tokio_util::sync::CancellationToken;
use uuid::Uuid;

use crate::config::DatabaseConfig;

const IDLE_TIMEOUT: Duration = Duration::from_secs(10 * 60);
const MAX_LIFETIME: Duration = Duration::from_secs(30 * 60);
pub(crate) const RUN_EVENT_NOTIFICATION_CHANNEL: &str = "agent_run_events_v1";

/// Migrations owned by Agent.
pub static MIGRATOR: sqlx::migrate::Migrator = sqlx::migrate!("./migrations");

#[derive(Clone)]
pub struct Database {
    pool: PgPool,
    run_events: broadcast::Sender<Uuid>,
}

impl Database {
    pub async fn connect(config: &DatabaseConfig) -> Result<Self, sqlx::Error> {
        let pool = PgPoolOptions::new()
            .min_connections(config.min_connections)
            .max_connections(config.max_connections)
            .acquire_timeout(config.acquire_timeout)
            .idle_timeout(IDLE_TIMEOUT)
            .max_lifetime(MAX_LIFETIME)
            .after_connect(|connection, _metadata| {
                Box::pin(async move {
                    sqlx::query("set application_name = 'agent'")
                        .execute(&mut *connection)
                        .await?;
                    sqlx::query("set statement_timeout = '30s'")
                        .execute(&mut *connection)
                        .await?;
                    sqlx::query("set lock_timeout = '5s'")
                        .execute(&mut *connection)
                        .await?;
                    sqlx::query("set idle_in_transaction_session_timeout = '15s'")
                        .execute(&mut *connection)
                        .await?;
                    Ok(())
                })
            })
            .connect(&config.url)
            .await?;

        Ok(Self::from_pool(pool))
    }

    #[must_use]
    pub fn from_pool(pool: PgPool) -> Self {
        let (run_events, _) = broadcast::channel(1_024);
        Self { pool, run_events }
    }

    pub async fn migrate(&self) -> Result<(), MigrateError> {
        migrate(&self.pool).await
    }

    pub async fn health_check(&self) -> Result<(), sqlx::Error> {
        sqlx::query("select 1").execute(&self.pool).await?;
        Ok(())
    }

    #[must_use]
    pub const fn pool(&self) -> &PgPool {
        &self.pool
    }

    pub(crate) fn subscribe_run_events(&self) -> broadcast::Receiver<Uuid> {
        self.run_events.subscribe()
    }

    /// Relays transactionally committed run-event notifications from PostgreSQL
    /// into this Agent process. Every Agent instance starts its own listener, so
    /// an SSE connection is woken even when another instance wrote the event.
    pub async fn spawn_run_event_listener(
        &self,
        shutdown: CancellationToken,
    ) -> Result<JoinHandle<()>, sqlx::Error> {
        let mut listener = PgListener::connect_with(&self.pool).await?;
        listener.listen(RUN_EVENT_NOTIFICATION_CHANNEL).await?;
        let run_events = self.run_events.clone();
        Ok(tokio::spawn(async move {
            loop {
                tokio::select! {
                    () = shutdown.cancelled() => break,
                    notification = listener.recv() => match notification {
                        Ok(notification) => match notification.payload().parse::<Uuid>() {
                            Ok(run_id) => {
                                let _ = run_events.send(run_id);
                            }
                            Err(error) => tracing::warn!(
                                %error,
                                payload = notification.payload(),
                                "ignoring invalid run event notification"
                            ),
                        },
                        Err(error) => {
                            tracing::warn!(%error, "run event notification listener failed");
                            tokio::select! {
                                () = shutdown.cancelled() => break,
                                () = tokio::time::sleep(Duration::from_secs(1)) => {}
                            }
                        }
                    }
                }
            }
        }))
    }
}

pub async fn migrate(pool: &PgPool) -> Result<(), MigrateError> {
    MIGRATOR.run(pool).await
}
