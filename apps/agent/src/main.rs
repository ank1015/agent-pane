use std::error::Error;

use agent::{
    AppState, Database,
    broker::Broker,
    config::AppConfig,
    execution::{spawn_outbox_publisher, spawn_wait_expiry},
    router,
};
use tokio::{net::TcpListener, signal};
use tokio_util::sync::CancellationToken;
use tracing_subscriber::EnvFilter;

#[tokio::main]
async fn main() -> Result<(), Box<dyn Error>> {
    dotenvy::dotenv().ok();
    init_tracing();

    let config = AppConfig::from_env()?;
    let database = Database::connect(&config.database).await?;
    database.migrate().await?;
    let broker = Broker::connect(&config.nats.url).await?;
    broker.ensure_topology().await?;

    let shutdown = CancellationToken::new();
    let _commands = broker
        .spawn_command_consumer(database.clone(), shutdown.clone())
        .await?;
    let _outbox = spawn_outbox_publisher(
        database.clone(),
        broker.context(),
        config.nats.outbox_interval,
        config.nats.outbox_batch_size,
        shutdown.clone(),
    );
    let _wait_expiry = spawn_wait_expiry(
        database.clone(),
        config.nats.wait_expiry_interval,
        config.nats.wait_expiry_batch_size,
        shutdown.clone(),
    );

    let listener = TcpListener::bind(config.bind_address).await?;
    tracing::info!(address = %config.bind_address, "agent listening");

    let state = AppState::new(
        database,
        config.control_token,
        config.harness_token,
        config.execution_policy,
    )
    .with_broker_client(broker.client());
    axum::serve(listener, router(state, config.max_request_bytes))
        .with_graceful_shutdown(async move {
            shutdown_signal().await;
            shutdown.cancel();
        })
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
