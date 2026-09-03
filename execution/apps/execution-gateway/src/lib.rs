mod config;
mod crypto;
mod database;
mod error;
mod host_connections;
mod http;
mod lifecycle;
mod provider;
mod security;

use std::sync::Arc;

pub use config::{Config, ConfigError};
pub use crypto::CredentialVault;
pub use database::{Database, HostFailure};
pub use host_connections::HostConnections;
pub use http::{AppState, router};
pub use lifecycle::LifecycleReconciler;
pub use provider::{
    DynE2bProvider, E2bProvider, E2bProviderSettings, ProviderExecutionResponse,
    ProviderHostDetails, ProviderHostState, RealE2bProvider,
};
pub use security::SecurityControls;

pub async fn build(config: &Config) -> anyhow::Result<axum::Router> {
    let database = Database::connect(&config.database_url, config.database_max_connections).await?;
    database.migrate().await?;
    database.mark_registered_hosts_unavailable().await?;
    let provider: DynE2bProvider = Arc::new(RealE2bProvider::new(E2bProviderSettings {
        control_base_url: config.e2b_control_base_url.clone(),
        envd_base_url_override: config.e2b_envd_base_url_override.clone(),
        request_timeout: config.e2b_request_timeout,
    }));
    let reconciler =
        LifecycleReconciler::new(database.clone(), config.vault.clone(), provider.clone());
    reconciler.clone().start(config.reconcile_interval);
    Ok(router(
        AppState {
            database,
            vault: config.vault.clone(),
            provider,
            reconciler,
            api_token: Arc::from(config.api_token.clone()),
            default_host_timeout_seconds: config.default_host_timeout_seconds,
            public_base_url: config.public_base_url.clone(),
            registration_token_ttl: config.registration_token_ttl,
            heartbeat_interval: config.registered_heartbeat_interval,
            connections: HostConnections::new(
                config.registered_operation_timeout,
                config.registered_max_in_flight,
            ),
            security: SecurityControls::new(
                config.max_operations_in_flight,
                config.max_authentication_failures_per_minute,
                config.max_machine_entry_attempts_per_minute,
            ),
        },
        config.max_request_bytes,
    ))
}
