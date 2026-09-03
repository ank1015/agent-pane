mod config;
mod connection;
mod credential;
mod identity;
mod registration;

use std::{path::PathBuf, sync::Arc};

use anyhow::Context as _;
use clap::{Parser, Subcommand};
use config::HostConfig;
use execution_core::ExecutionRuntime as _;
use execution_supervisor_core::SupervisorRuntime;
use tracing_subscriber::EnvFilter;

#[derive(Debug, Parser)]
#[command(name = "execution-host", version, about = "Registered Host daemon")]
struct Cli {
    #[arg(
        long,
        env = "EXECUTION_HOST_CONFIG",
        default_value = "execution-host.json"
    )]
    config: PathBuf,
    #[command(subcommand)]
    command: Command,
}

#[derive(Debug, Subcommand)]
enum Command {
    /// Exchange a one-time registration token for a persistent host credential.
    Register {
        #[arg(long, env = "EXECUTION_HOST_REGISTRATION_TOKEN")]
        token: String,
    },
    /// Maintain the outbound connection and serve execution operations.
    Connect,
    /// Validate local configuration and stored identity/credential state.
    Doctor,
    /// Print the descriptor for a new local supervisor generation.
    Describe,
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info")),
        )
        .init();
    let cli = Cli::parse();
    let config = HostConfig::load(&cli.config)
        .await
        .with_context(|| format!("load Host Daemon config {}", cli.config.display()))?;
    let installation_id = identity::load_or_create(&config.state_directory)
        .await
        .context("load Host Daemon installation identity")?;

    match cli.command {
        Command::Register { token } => {
            let claimed = registration::register(
                &config.gateway,
                token,
                installation_id,
                &config.state_directory,
                config.allow_insecure_http,
            )
            .await?;
            println!("registered execution host {}", claimed.host_id);
        }
        Command::Connect => {
            let credential = credential::load(&config.state_directory)
                .await
                .context("load Host Daemon credential")?
                .context("host is not registered; run `execution-host register` first")?;
            let runtime = Arc::new(
                SupervisorRuntime::new(config.supervisor_config(credential.host_id)?)
                    .await
                    .context("start embedded execution supervisor")?,
            );
            tokio::select! {
                result = connection::run(Arc::clone(&runtime), credential) => result?,
                _ = tokio::signal::ctrl_c() => {}
            }
            runtime.shutdown().await;
        }
        Command::Doctor => {
            let credential = credential::load(&config.state_directory)
                .await
                .context("load Host Daemon credential")?;
            println!("configuration: ok");
            println!("installation_id: {installation_id}");
            println!(
                "registration: {}",
                credential
                    .map(|value| value.host_id.to_string())
                    .unwrap_or_else(|| "not registered".to_owned())
            );
        }
        Command::Describe => {
            let credential = credential::load(&config.state_directory)
                .await
                .context("load Host Daemon credential")?
                .context("host is not registered; run `execution-host register` first")?;
            let runtime = SupervisorRuntime::new(config.supervisor_config(credential.host_id)?)
                .await
                .context("start embedded execution supervisor")?;
            println!("{}", serde_json::to_string_pretty(runtime.descriptor())?);
            runtime.shutdown().await;
        }
    }
    Ok(())
}
