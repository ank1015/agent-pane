use std::error::Error;

use llm_gateway::{
    account::AccountService,
    config::AppConfig,
    db::Database,
    gateway::{ConcurrencyConfig, Gateway},
    http,
};
use tokio::{net::TcpListener, signal};
use tracing_subscriber::EnvFilter;

#[tokio::main]
async fn main() -> Result<(), Box<dyn Error>> {
    dotenvy::dotenv().ok();
    init_tracing();
    let config = AppConfig::from_env()?;
    serve(config).await?;
    Ok(())
}

async fn serve(config: AppConfig) -> Result<(), Box<dyn Error>> {
    let database = Database::connect(&config.database).await?;
    database.migrate().await?;
    let accounts = AccountService::new(database.clone(), config.vault.clone());
    let gateway = Gateway::new(
        database.clone(),
        accounts.clone(),
        ConcurrencyConfig {
            max_active_requests: config.max_concurrent_requests,
            max_waiting_requests: config.max_queued_requests,
            wait_timeout: config.queue_timeout,
        },
        config.request_timeout,
        config.chatgpt_oauth_client_id.clone(),
        config.chatgpt_oauth_token_url.clone(),
    )?;

    let listener = TcpListener::bind(config.bind_address).await?;
    tracing::info!(
        address = %config.bind_address,
        max_concurrent_requests = config.max_concurrent_requests,
        max_queued_requests = config.max_queued_requests,
        queue_timeout_seconds = config.queue_timeout.as_secs(),
        max_request_bytes = config.max_request_bytes,
        request_timeout_seconds = config.request_timeout.as_secs(),
        "llm gateway listening"
    );

    axum::serve(
        listener,
        http::router(
            database,
            gateway,
            accounts,
            config.api_token,
            config.admin_token,
            config.max_request_bytes,
            config.max_authentication_failures_per_minute,
        ),
    )
    .with_graceful_shutdown(shutdown_signal())
    .await?;

    Ok(())
}

fn init_tracing() {
    let filter =
        EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("llm_gateway=info"));
    tracing_subscriber::fmt().with_env_filter(filter).init();
}

async fn shutdown_signal() {
    match signal::ctrl_c().await {
        Ok(()) => tracing::info!("shutdown signal received"),
        Err(error) => tracing::error!(%error, "could not listen for shutdown signal"),
    }
}
