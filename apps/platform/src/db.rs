use std::time::Duration;

use sqlx::{PgPool, migrate::MigrateError, postgres::PgPoolOptions};

use crate::config::DatabaseConfig;

const IDLE_TIMEOUT: Duration = Duration::from_secs(10 * 60);
const MAX_LIFETIME: Duration = Duration::from_secs(30 * 60);

/// Migrations owned by Platform.
pub static MIGRATOR: sqlx::migrate::Migrator = sqlx::migrate!("./migrations");

#[derive(Clone)]
pub struct Database {
    pool: PgPool,
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
                    sqlx::query("set application_name = 'platform'")
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

        Ok(Self { pool })
    }

    #[must_use]
    pub const fn from_pool(pool: PgPool) -> Self {
        Self { pool }
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
}

pub async fn migrate(pool: &PgPool) -> Result<(), MigrateError> {
    MIGRATOR.run(pool).await
}
