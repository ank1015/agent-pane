use std::error::Error;

use agent_harness_sdk::HarnessServer;
use execution_runtime::OperationContext;
use pi_harness::{
    clients::{PI_HARNESS_DESCRIPTOR, PI_HARNESS_REVISION_ID, ensure_pi_harness},
    config::HarnessConfig,
    runtime::PiRuntime,
};
use tokio::signal;
use tracing_subscriber::EnvFilter;

#[tokio::main]
async fn main() -> Result<(), Box<dyn Error>> {
    dotenvy::dotenv().ok();
    init_tracing();

    let config = HarnessConfig::from_env()?;
    if let Some(registration) = config.harness_registration.clone() {
        ensure_pi_harness(registration).await?;
        tracing::info!(
            harness_revision_id = PI_HARNESS_REVISION_ID,
            "Pi harness registered with Agent"
        );
    }
    let runtime = PiRuntime::from_config(&config)?;
    let server =
        HarnessServer::connect(&config.server_config(), PI_HARNESS_DESCRIPTOR, runtime).await?;
    tracing::info!(instance_id = %config.instance_id, max_concurrent_turns = config.max_concurrent_turns, "Pi harness server started");

    let shutdown = OperationContext::new();
    let signal_shutdown = shutdown.clone();
    let signal_task = tokio::spawn(async move {
        match signal::ctrl_c().await {
            Ok(()) => tracing::info!("shutdown signal received"),
            Err(error) => tracing::error!(%error, "could not listen for shutdown signal"),
        }
        signal_shutdown.cancel();
    });
    let result = server.run(&shutdown).await;
    signal_task.abort();
    result?;
    Ok(())
}

fn init_tracing() {
    let filter =
        EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("pi_harness=info"));
    tracing_subscriber::fmt().with_env_filter(filter).init();
}
