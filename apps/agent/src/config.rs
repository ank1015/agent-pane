use std::{env, net::SocketAddr, num::ParseIntError, time::Duration};

use crate::{
    auth::{ControlToken, ControlTokenError, WorkerToken, WorkerTokenError},
    execution::{ExecutionPolicy, ExecutionPolicyError},
};

const DEFAULT_BIND_ADDRESS: &str = "127.0.0.1:8780";
const DEFAULT_MAX_CONNECTIONS: u32 = 20;
const DEFAULT_MIN_CONNECTIONS: u32 = 1;
const DEFAULT_ACQUIRE_TIMEOUT_SECONDS: u64 = 5;
const DEFAULT_MAX_REQUEST_BYTES: usize = 1024 * 1024;
const DEFAULT_MAX_TURNS: u32 = 100;
const MAX_TURNS: u32 = 1_000;
const DEFAULT_MAX_FAILURES_PER_TURN: u32 = 3;
const MAX_FAILURES_PER_TURN: u32 = 10;
const DEFAULT_WORKER_LEASE_SECONDS: u32 = 30;
const DEFAULT_WORKER_REAPER_INTERVAL_SECONDS: u64 = 5;
const DEFAULT_WORKER_MAX_SUPPORTED_REVISIONS: usize = 256;
const DEFAULT_WORKER_MAX_MESSAGE_BATCH: usize = 100;
const DEFAULT_WORKER_REAPER_BATCH_SIZE: u32 = 100;
const DEFAULT_ABORT_GRACE_SECONDS: u32 = 30;

#[derive(Clone)]
pub struct AppConfig {
    pub bind_address: SocketAddr,
    pub database: DatabaseConfig,
    pub control_token: ControlToken,
    pub worker_token: WorkerToken,
    pub max_request_bytes: usize,
    pub execution_policy: ExecutionPolicy,
}

#[derive(Clone)]
pub struct DatabaseConfig {
    pub(crate) url: String,
    pub(crate) max_connections: u32,
    pub(crate) min_connections: u32,
    pub(crate) acquire_timeout: Duration,
}

impl AppConfig {
    pub fn from_env() -> Result<Self, ConfigError> {
        let bind_address = env::var("AGENT_BIND_ADDRESS")
            .unwrap_or_else(|_| DEFAULT_BIND_ADDRESS.to_owned())
            .parse()
            .map_err(ConfigError::InvalidBindAddress)?;
        let database_url = required("AGENT_DATABASE_URL")?;
        let control_token = ControlToken::new(required("AGENT_CONTROL_TOKEN")?)?;
        let worker_token = WorkerToken::new(required("AGENT_WORKER_TOKEN")?)?;
        let max_connections = parse_u32("AGENT_DATABASE_MAX_CONNECTIONS", DEFAULT_MAX_CONNECTIONS)?;
        let min_connections = parse_u32("AGENT_DATABASE_MIN_CONNECTIONS", DEFAULT_MIN_CONNECTIONS)?;
        let acquire_timeout_seconds = parse_u64(
            "AGENT_DATABASE_ACQUIRE_TIMEOUT_SECONDS",
            DEFAULT_ACQUIRE_TIMEOUT_SECONDS,
        )?;
        let max_request_bytes = parse_usize("AGENT_MAX_REQUEST_BYTES", DEFAULT_MAX_REQUEST_BYTES)?;
        let execution_policy = ExecutionPolicy::new(
            parse_u32("AGENT_DEFAULT_MAX_TURNS", DEFAULT_MAX_TURNS)?,
            parse_u32("AGENT_MAX_TURNS", MAX_TURNS)?,
            parse_u32(
                "AGENT_DEFAULT_MAX_FAILURES_PER_TURN",
                DEFAULT_MAX_FAILURES_PER_TURN,
            )?,
            parse_u32("AGENT_MAX_FAILURES_PER_TURN", MAX_FAILURES_PER_TURN)?,
        )?
        .with_worker_settings(
            parse_u32("AGENT_WORKER_LEASE_SECONDS", DEFAULT_WORKER_LEASE_SECONDS)?,
            parse_u64(
                "AGENT_WORKER_REAPER_INTERVAL_SECONDS",
                DEFAULT_WORKER_REAPER_INTERVAL_SECONDS,
            )?,
            parse_usize(
                "AGENT_WORKER_MAX_SUPPORTED_REVISIONS",
                DEFAULT_WORKER_MAX_SUPPORTED_REVISIONS,
            )?,
            parse_usize(
                "AGENT_WORKER_MAX_MESSAGE_BATCH",
                DEFAULT_WORKER_MAX_MESSAGE_BATCH,
            )?,
            parse_u32(
                "AGENT_WORKER_REAPER_BATCH_SIZE",
                DEFAULT_WORKER_REAPER_BATCH_SIZE,
            )?,
        )?
        .with_abort_grace_seconds(parse_u32(
            "AGENT_ABORT_GRACE_SECONDS",
            DEFAULT_ABORT_GRACE_SECONDS,
        )?)?;

        if max_connections == 0 {
            return Err(ConfigError::NotPositive("AGENT_DATABASE_MAX_CONNECTIONS"));
        }
        if min_connections > max_connections {
            return Err(ConfigError::MinConnectionsExceedsMax {
                min: min_connections,
                max: max_connections,
            });
        }
        if acquire_timeout_seconds == 0 {
            return Err(ConfigError::NotPositive(
                "AGENT_DATABASE_ACQUIRE_TIMEOUT_SECONDS",
            ));
        }
        if max_request_bytes == 0 {
            return Err(ConfigError::NotPositive("AGENT_MAX_REQUEST_BYTES"));
        }

        Ok(Self {
            bind_address,
            database: DatabaseConfig {
                url: database_url,
                max_connections,
                min_connections,
                acquire_timeout: Duration::from_secs(acquire_timeout_seconds),
            },
            control_token,
            worker_token,
            max_request_bytes,
            execution_policy,
        })
    }
}

fn required(name: &'static str) -> Result<String, ConfigError> {
    let value = env::var(name).map_err(|_| ConfigError::Missing(name))?;
    if value.trim().is_empty() {
        return Err(ConfigError::Missing(name));
    }
    Ok(value)
}

fn parse_u32(name: &'static str, default: u32) -> Result<u32, ConfigError> {
    parse_integer(name, default)
}

fn parse_u64(name: &'static str, default: u64) -> Result<u64, ConfigError> {
    parse_integer(name, default)
}

fn parse_usize(name: &'static str, default: usize) -> Result<usize, ConfigError> {
    parse_integer(name, default)
}

fn parse_integer<T>(name: &'static str, default: T) -> Result<T, ConfigError>
where
    T: std::str::FromStr<Err = ParseIntError>,
{
    env::var(name).map_or_else(
        |_| Ok(default),
        |value| {
            value.parse().map_err(|source| ConfigError::InvalidInteger {
                name,
                value,
                source,
            })
        },
    )
}

#[derive(Debug, thiserror::Error)]
pub enum ConfigError {
    #[error("required environment variable {0} is missing or empty")]
    Missing(&'static str),
    #[error("AGENT_BIND_ADDRESS is not a valid socket address")]
    InvalidBindAddress(#[source] std::net::AddrParseError),
    #[error("{name} must be an integer, got {value:?}")]
    InvalidInteger {
        name: &'static str,
        value: String,
        #[source]
        source: ParseIntError,
    },
    #[error("{0} must be greater than zero")]
    NotPositive(&'static str),
    #[error(
        "AGENT_DATABASE_MIN_CONNECTIONS ({min}) cannot exceed AGENT_DATABASE_MAX_CONNECTIONS ({max})"
    )]
    MinConnectionsExceedsMax { min: u32, max: u32 },
    #[error(transparent)]
    ControlToken(#[from] ControlTokenError),
    #[error(transparent)]
    WorkerToken(#[from] WorkerTokenError),
    #[error(transparent)]
    ExecutionPolicy(#[from] ExecutionPolicyError),
}
