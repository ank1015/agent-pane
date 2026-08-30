use std::{error::Error, sync::Arc};

use agent_harness_sdk::HarnessServer;
use codex_harness::{
    CODEX_HARNESS_DESCRIPTOR, CODEX_HARNESS_REVISION_ID, CodexRuntime,
    config::HarnessConfig,
    ensure_codex_harness,
    persistence::{CodexToolState, Database},
};
use execution_runtime::OperationContext;
use tokio::signal;
use tracing_subscriber::EnvFilter;

#[tokio::main]
async fn main() -> Result<(), Box<dyn Error>> {
    dotenvy::dotenv().ok();
    init_tracing();

    let config = HarnessConfig::from_env()?;
    let database = Database::connect(&config.database).await?;
    database.migrate().await?;
    let tool_state = Arc::new(CodexToolState::new(
        database.pool().clone(),
        config.code_mode_idle_ttl,
    ));
    if let Some(registration) = config.harness_registration.clone() {
        ensure_codex_harness(registration).await?;
        tracing::info!(
            harness_revision_id = CODEX_HARNESS_REVISION_ID,
            "Codex harness registered with Agent"
        );
    }

    let runtime = CodexRuntime::from_config(&config, Arc::clone(&tool_state))?;
    let server =
        HarnessServer::connect(&config.server_config(), CODEX_HARNESS_DESCRIPTOR, runtime).await?;
    tracing::info!(
        instance_id = %config.instance_id,
        max_concurrent_turns = config.max_concurrent_turns,
        "Codex harness server started"
    );

    let shutdown = OperationContext::new();
    let reaper_state = tool_state.clone();
    let reaper_shutdown = shutdown.clone();
    let reaper_task = tokio::spawn(async move {
        reaper_state.run_code_mode_reaper(&reaper_shutdown).await;
    });
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
    reaper_task.abort();
    tool_state.shutdown().await;
    database.close().await;
    result?;
    Ok(())
}

fn init_tracing() {
    let filter =
        EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("codex_harness=info"));
    tracing_subscriber::fmt().with_env_filter(filter).init();
}
