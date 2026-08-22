use std::error::Error;

use clap::Parser;
use llm_gateway::{
    account::AccountService,
    cli::{Cli, Command, run_account},
    config::AppConfig,
    db::Database,
    gateway::Gateway,
    http,
};
use tokio::{net::TcpListener, signal};
use tracing_subscriber::EnvFilter;

#[tokio::main]
async fn main() -> Result<(), Box<dyn Error>> {
    dotenvy::dotenv().ok();
    init_tracing();

    let cli = Cli::parse();
    let config = AppConfig::from_env()?;
    match cli.command.unwrap_or(Command::Serve) {
        Command::Serve => serve(config).await?,
        Command::Account(args) => run_account(args.command, &config).await?,
    }

    Ok(())
}

async fn serve(config: AppConfig) -> Result<(), Box<dyn Error>> {
    let database = Database::connect(&config.database).await?;
    database.migrate().await?;
    let accounts = AccountService::new(database.clone(), config.vault.clone());
    let gateway = Gateway::new(
        database.clone(),
        accounts.clone(),
        config.max_concurrent_requests,
        config.request_timeout,
        config.chatgpt_oauth_client_id.clone(),
        config.chatgpt_oauth_token_url.clone(),
    );

    let listener = TcpListener::bind(config.bind_address).await?;
    tracing::info!(
        address = %config.bind_address,
        max_concurrent_requests = config.max_concurrent_requests,
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
            config.admin_token,
            config.max_request_bytes,
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
