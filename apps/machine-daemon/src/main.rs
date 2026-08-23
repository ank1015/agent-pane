use std::{net::SocketAddr, path::PathBuf, sync::Arc};

use anyhow::Context;
use clap::{Parser, Subcommand};
use execution_local::LocalExecutionEnvironment;
use execution_runtime::ExecutionEnvironment;
use machine_daemon::{config::DaemonConfig, credential, doctor, identity, registration, transport};
use tracing_subscriber::EnvFilter;

#[derive(Debug, Parser)]
#[command(
    name = "machine-daemon",
    about = "Expose this device as an execution environment"
)]
struct Cli {
    #[arg(
        long,
        env = "MACHINE_DAEMON_CONFIG",
        default_value = "machine-daemon.json"
    )]
    config: PathBuf,
    #[command(subcommand)]
    command: Command,
}

#[derive(Debug, Subcommand)]
enum Command {
    /// Exchange a one-time registration token for this machine's cloud credential.
    Register {
        #[arg(long)]
        gateway: String,
        #[arg(
            long,
            env = "MACHINE_DAEMON_REGISTRATION_TOKEN",
            hide_env_values = true
        )]
        token: String,
    },
    /// Listen for inbound WebSocket connections.
    Serve {
        #[arg(long)]
        listen: Option<SocketAddr>,
    },
    /// Maintain an outbound WebSocket connection to a gateway.
    Connect {
        #[arg(long)]
        gateway: Option<String>,
    },
    /// Serve newline-delimited JSON over stdin/stdout for SSH and sandbox launchers.
    Stdio,
    /// Print the environment descriptor as JSON.
    Describe,
    /// Validate roots, state storage, shell execution, and gateway configuration.
    Doctor,
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    init_tracing();
    let cli = Cli::parse();
    let config = DaemonConfig::load(&cli.config).await?;
    let machine_id =
        identity::load_or_create(&config.state_directory, config.machine_id.as_deref()).await?;
    let environment = Arc::new(
        LocalExecutionEnvironment::new(config.local_config(machine_id)?)
            .await
            .context("initialize local execution environment")?,
    );

    match cli.command {
        Command::Register { gateway, token } => {
            let claimed = registration::register(
                &gateway,
                token,
                environment.descriptor(),
                &config.state_directory,
            )
            .await?;
            println!("registered machine {}", claimed.machine_id);
        }
        Command::Serve { listen } => {
            transport::serve::run(
                environment,
                listen.unwrap_or(config.listen),
                config.auth_token()?,
            )
            .await?;
        }
        Command::Connect { gateway } => {
            let cloud = credential::load(&config.state_directory)
                .await?
                .context("machine is not registered; run `machine-daemon register` first")?;
            if cloud.machine_id != environment.descriptor().machine_id {
                anyhow::bail!("stored cloud credential belongs to a different machine identity");
            }
            transport::connect::run(
                environment,
                gateway.unwrap_or(cloud.websocket_url),
                Some(cloud.credential),
            )
            .await?;
        }
        Command::Stdio => transport::stdio::run(environment).await?,
        Command::Describe => {
            println!(
                "{}",
                serde_json::to_string_pretty(environment.descriptor())?
            );
        }
        Command::Doctor => {
            let report = doctor::run(
                environment,
                &config.state_directory,
                config.gateway.as_deref(),
            )
            .await;
            println!("{}", serde_json::to_string_pretty(&report)?);
            if !report.healthy {
                anyhow::bail!("one or more doctor checks failed");
            }
        }
    }
    Ok(())
}

fn init_tracing() {
    let filter =
        EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("machine_daemon=info"));
    tracing_subscriber::fmt()
        .with_env_filter(filter)
        .with_writer(std::io::stderr)
        .init();
}
