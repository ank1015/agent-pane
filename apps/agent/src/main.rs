use std::error::Error;

use agent::{AppState, Database, config::AppConfig, execution::spawn_reaper, router};
use tokio::{net::TcpListener, signal};
use tracing_subscriber::EnvFilter;

#[tokio::main]
async fn main() -> Result<(), Box<dyn Error>> {
    dotenvy::dotenv().ok();
    init_tracing();

    let config = AppConfig::from_env()?;
    let database = Database::connect(&config.database).await?;
    database.migrate().await?;

    let listener = TcpListener::bind(config.bind_address).await?;
    tracing::info!(address = %config.bind_address, "agent listening");

    let _reaper = spawn_reaper(database.clone(), config.execution_policy);
    let state = AppState::new(
        database,
        config.control_token,
        config.worker_token,
        config.execution_policy,
    );
    axum::serve(listener, router(state, config.max_request_bytes))
        .with_graceful_shutdown(shutdown_signal())
        .await?;

    Ok(())
}

fn init_tracing() {
    let filter = EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("agent=info"));
    tracing_subscriber::fmt().with_env_filter(filter).init();
}

async fn shutdown_signal() {
    match signal::ctrl_c().await {
        Ok(()) => tracing::info!("shutdown signal received"),
        Err(error) => tracing::error!(%error, "could not listen for shutdown signal"),
    }
}
