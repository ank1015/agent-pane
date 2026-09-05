mod accounts;
mod models;
mod requests;

use std::time::Duration;

pub use accounts::{AccountPatch, AccountStoreError, NewProviderAccount};
pub use models::{AdminAccount, ProviderAccount, ResolvedAccount};
pub use requests::{
    LlmOperation, LlmRequestFilters, LlmRequestPage, LlmRequestRecord, RequestQueryError,
    UsageGroupBy, UsageReport,
};
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
            .min_connections(config.min_connections)
            .max_connections(config.max_connections)
            .acquire_timeout(config.acquire_timeout)
            .idle_timeout(IDLE_TIMEOUT)
            .max_lifetime(MAX_LIFETIME)
            .after_connect(|connection, _metadata| {
                Box::pin(async move {
                    sqlx::query("set application_name = 'llm-gateway'")
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

    pub async fn migrate(&self) -> Result<(), sqlx::migrate::MigrateError> {
        sqlx::migrate!("./migrations").run(&self.pool).await
    }

    /// Remove expired payloads, retaining deduplication tombstones for seven days.
    pub async fn cleanup_runs(&self) -> Result<(), sqlx::Error> {
        sqlx::query("update llm_runs set result = null, status = 'expired' where expires_at <= now() and status <> 'expired'")
            .execute(&self.pool).await?;
        sqlx::query("delete from llm_runs where expires_at <= now() - interval '5 days'")
            .execute(&self.pool)
            .await?;
        Ok(())
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

#[cfg(test)]
mod tests {
    #[test]
    fn initial_migration_defines_exactly_the_four_application_tables() {
        let migration = include_str!("../../migrations/20260903000000_create_llm_gateway.sql");
        for table in [
            "provider_accounts",
            "provider_credentials",
            "llm_requests",
            "llm_usage",
        ] {
            assert!(migration.contains(&format!("create table {table}")));
        }
        assert_eq!(migration.matches("create table ").count(), 4);
    }
}
