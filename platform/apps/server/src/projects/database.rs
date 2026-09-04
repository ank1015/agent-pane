use std::time::Duration;

use sqlx::{PgPool, migrate::MigrateError, postgres::PgPoolOptions};

static MIGRATOR: sqlx::migrate::Migrator = sqlx::migrate!("./migrations");

#[derive(Clone)]
pub struct ProjectDatabase {
    pool: PgPool,
}

impl ProjectDatabase {
    pub async fn connect(
        url: &str,
        min_connections: u32,
        max_connections: u32,
        acquire_timeout: Duration,
    ) -> Result<Self, sqlx::Error> {
        let pool = PgPoolOptions::new()
            .min_connections(min_connections)
            .max_connections(max_connections)
            .acquire_timeout(acquire_timeout)
            .idle_timeout(Duration::from_secs(10 * 60))
            .max_lifetime(Duration::from_secs(30 * 60))
            .after_connect(|connection, _| {
                Box::pin(async move {
                    sqlx::query("set application_name = 'platform-server'")
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
            .connect(url)
            .await?;
        Ok(Self { pool })
    }

    pub async fn migrate(&self) -> Result<(), MigrateError> {
        // This standalone server owns its database and complete migration history.
        MIGRATOR.run(&self.pool).await
    }

    pub fn pool(&self) -> &PgPool {
        &self.pool
    }
}
