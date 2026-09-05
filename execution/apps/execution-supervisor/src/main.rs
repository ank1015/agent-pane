use std::path::PathBuf;

use anyhow::{Context, Result};
use clap::{Args, Parser, Subcommand};
use execution_core::{ExecutionHostId, OperationContext, RootId};
use execution_supervisor::{ServeConfig, probe, relay_stdio, serve};
use execution_supervisor_core::{SupervisorConfig, SupervisorLimits, SupervisorRoot};
use execution_wire::{PROTOCOL_NAME, PROTOCOL_VERSION};
use serde::Serialize;
use tracing_subscriber::EnvFilter;

const DEFAULT_SOCKET_PATH: &str = "/tmp/agent-pane-execution/supervisor.sock";
const DEFAULT_STATE_DIRECTORY: &str = "/tmp/agent-pane-execution/state";

#[derive(Debug, Parser)]
#[command(
    name = "execution-supervisor",
    about = "Serve filesystem and durable process operations over local IPC"
)]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Debug, Subcommand)]
enum Command {
    /// Start one supervisor generation and serve a local Unix socket.
    Serve(ServeArgs),
    /// Relay one NDJSON request from stdin to a running supervisor.
    Rpc(RpcArgs),
    /// Probe a running supervisor and print its descriptor.
    Health(SocketArgs),
    /// Print executable and protocol version information.
    Version,
}

#[derive(Debug, Args)]
struct SocketArgs {
    #[arg(
        long,
        env = "EXECUTION_SUPERVISOR_SOCKET",
        default_value = DEFAULT_SOCKET_PATH
    )]
    socket: PathBuf,
}

#[derive(Debug, Args)]
struct RpcArgs {
    #[arg(
        long,
        env = "EXECUTION_SUPERVISOR_SOCKET",
        default_value = DEFAULT_SOCKET_PATH
    )]
    socket: PathBuf,
    /// Bounds the complete stdin-to-socket-to-stdout relay so an interrupted
    /// provider stream cannot leave a relay process waiting forever.
    #[arg(long)]
    timeout_ms: Option<u64>,
}

#[derive(Debug, Args)]
struct ServeArgs {
    #[arg(long, env = "EXECUTION_HOST_ID")]
    host_id: String,
    #[arg(long, env = "EXECUTION_ROOT")]
    root: PathBuf,
    #[arg(long, default_value = "workspace")]
    root_id: String,
    #[arg(long, default_value = "Workspace")]
    root_name: String,
    #[arg(long)]
    read_only: bool,
    #[arg(
        long,
        env = "EXECUTION_SUPERVISOR_SOCKET",
        default_value = DEFAULT_SOCKET_PATH
    )]
    socket: PathBuf,
    #[arg(
        long,
        env = "EXECUTION_SUPERVISOR_STATE_DIR",
        default_value = DEFAULT_STATE_DIRECTORY
    )]
    state_dir: PathBuf,
}

#[derive(Serialize)]
struct HealthReport {
    status: &'static str,
    supervisor_version: &'static str,
    protocol: &'static str,
    protocol_version: u32,
    descriptor: execution_core::ExecutionHostDescriptor,
}

#[derive(Serialize)]
struct VersionReport {
    program: &'static str,
    version: &'static str,
    protocol: &'static str,
    protocol_version: u32,
}

#[tokio::main]
async fn main() -> Result<()> {
    init_tracing();
    match Cli::parse().command {
        Command::Serve(args) => run_serve(args).await,
        Command::Rpc(args) => {
            if let Some(timeout_ms) = args.timeout_ms {
                if timeout_ms == 0 {
                    anyhow::bail!("--timeout-ms must be greater than zero");
                }
                tokio::time::timeout(
                    std::time::Duration::from_millis(timeout_ms),
                    relay_stdio(&args.socket),
                )
                .await
                .context("supervisor RPC relay timed out")??;
                Ok(())
            } else {
                relay_stdio(&args.socket).await
            }
        }
        Command::Health(args) => {
            let descriptor = probe(&args.socket).await?;
            print_json(&HealthReport {
                status: "ok",
                supervisor_version: env!("CARGO_PKG_VERSION"),
                protocol: PROTOCOL_NAME,
                protocol_version: PROTOCOL_VERSION,
                descriptor,
            })
        }
        Command::Version => print_json(&VersionReport {
            program: "execution-supervisor",
            version: env!("CARGO_PKG_VERSION"),
            protocol: PROTOCOL_NAME,
            protocol_version: PROTOCOL_VERSION,
        }),
    }
}

async fn run_serve(args: ServeArgs) -> Result<()> {
    let host_id = ExecutionHostId::new(args.host_id)
        .map_err(anyhow::Error::msg)
        .context("invalid execution host ID")?;
    let root_id = RootId::new(args.root_id)
        .map_err(anyhow::Error::msg)
        .context("invalid execution root ID")?;
    let shutdown = OperationContext::new();
    let signal_shutdown = shutdown.clone();
    tokio::spawn(async move {
        match tokio::signal::ctrl_c().await {
            Ok(()) => tracing::info!("shutdown signal received"),
            Err(error) => tracing::error!(%error, "failed to listen for shutdown signal"),
        }
        signal_shutdown.cancel();
    });

    serve(
        ServeConfig {
            socket_path: args.socket,
            supervisor: SupervisorConfig {
                host_id,
                state_directory: args.state_dir,
                roots: vec![SupervisorRoot {
                    id: root_id,
                    name: args.root_name,
                    path: args.root,
                    read_only: args.read_only,
                }],
                limits: SupervisorLimits::default(),
            },
        },
        shutdown,
    )
    .await
}

fn print_json(value: &impl Serialize) -> Result<()> {
    println!(
        "{}",
        serde_json::to_string(value).context("serialize command output")?
    );
    Ok(())
}

fn init_tracing() {
    let filter = EnvFilter::try_from_default_env()
        .unwrap_or_else(|_| EnvFilter::new("execution_supervisor=info"));
    tracing_subscriber::fmt()
        .with_env_filter(filter)
        .with_writer(std::io::stderr)
        .init();
}
