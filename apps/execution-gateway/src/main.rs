use execution_gateway::{
    config::Config, connection_registry::ConnectionRegistry, connectors::ConnectorRouter,
    db::Database, routing::OperationRouter, sandbox_accounts::SandboxAccountService,
    sandbox_machines::SandboxSecretStore, sandbox_runtime::SandboxRuntimeFactory,
};
use tokio::net::TcpListener;
use tracing_subscriber::EnvFilter;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    init_tracing();
    let config = Config::from_env()?;
    let database = Database::connect(&config.database_url, config.database_max_connections).await?;
    database.migrate().await?;
    let connections = ConnectionRegistry::default();
    let accounts = SandboxAccountService::new(database.clone(), config.vault.clone());
    let secrets = SandboxSecretStore::new(database.clone(), config.vault.clone());
    let runtimes = SandboxRuntimeFactory::new(database.clone(), accounts, secrets);
    let connectors = ConnectorRouter::with_sandbox(connections.clone(), runtimes);
    let operation_router = OperationRouter::new(database.clone(), connectors);
    let router = execution_gateway::http::router(&config, database, connections, operation_router);
    let listener = TcpListener::bind(config.bind_address).await?;
    tracing::info!(address=%config.bind_address, "execution gateway listening");
    axum::serve(listener, router)
        .with_graceful_shutdown(shutdown())
        .await?;
    Ok(())
}

fn init_tracing() {
    let filter = EnvFilter::try_from_default_env()
        .unwrap_or_else(|_| EnvFilter::new("execution_gateway=info"));
    tracing_subscriber::fmt().with_env_filter(filter).init();
}
async fn shutdown() {
    let _ = tokio::signal::ctrl_c().await;
}
