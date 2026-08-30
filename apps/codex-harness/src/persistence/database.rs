use std::time::Duration;

use sqlx::{PgPool, postgres::PgPoolOptions};

use crate::config::DatabaseConfig;

const IDLE_TIMEOUT: Duration = Duration::from_secs(10 * 60);
const MAX_LIFETIME: Duration = Duration::from_secs(30 * 60);

#[derive(Clone)]
pub struct Database {
    pool: PgPool,
}

impl Database {
    pub async fn connect(config: &DatabaseConfig) -> Result<Self, sqlx::Error> {
        let pool = PgPoolOptions::new()
            .max_connections(config.max_connections)
            .acquire_timeout(config.acquire_timeout)
            .idle_timeout(IDLE_TIMEOUT)
            .max_lifetime(MAX_LIFETIME)
            .after_connect(|connection, _metadata| {
                Box::pin(async move {
                    sqlx::query("set application_name = 'codex-harness'")
                        .execute(&mut *connection)
                        .await?;
                    sqlx::query("set statement_timeout = '30s'")
                        .execute(&mut *connection)
                        .await?;
                    sqlx::query("set lock_timeout = '5s'")
                        .execute(&mut *connection)
                        .await?;
                    Ok(())
                })
            })
            .connect(&config.url)
            .await?;
        Ok(Self { pool })
    }

    pub async fn migrate(&self) -> Result<(), sqlx::migrate::MigrateError> {
        sqlx::migrate!("./migrations").run(&self.pool).await
    }

    #[must_use]
    pub const fn pool(&self) -> &PgPool {
        &self.pool
    }

    pub async fn close(&self) {
        self.pool.close().await;
    }
}
