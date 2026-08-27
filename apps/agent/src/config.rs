use std::{env, net::SocketAddr, num::ParseIntError, time::Duration};

use crate::{
    auth::{ControlToken, ControlTokenError, HarnessToken, HarnessTokenError},
    execution::{ExecutionPolicy, ExecutionPolicyError},
};

#[derive(Clone)]
pub struct AppConfig {
    pub bind_address: SocketAddr,
    pub database: DatabaseConfig,
    pub nats: NatsConfig,
    pub control_token: ControlToken,
    pub harness_token: HarnessToken,
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
#[derive(Clone)]
pub struct NatsConfig {
    pub url: String,
    pub outbox_interval: Duration,
    pub outbox_batch_size: i64,
    pub wait_expiry_interval: Duration,
    pub wait_expiry_batch_size: i64,
}
impl AppConfig {
    pub fn from_env() -> Result<Self, ConfigError> {
        let bind_address = env::var("AGENT_BIND_ADDRESS")
            .unwrap_or_else(|_| "127.0.0.1:8780".to_owned())
            .parse()
            .map_err(ConfigError::InvalidBindAddress)?;
        let max_connections = parse_u32("AGENT_DATABASE_MAX_CONNECTIONS", 20)?;
        let min_connections = parse_u32("AGENT_DATABASE_MIN_CONNECTIONS", 1)?;
        let acquire_seconds = parse_u64("AGENT_DATABASE_ACQUIRE_TIMEOUT_SECONDS", 5)?;
        positive("AGENT_DATABASE_MAX_CONNECTIONS", max_connections)?;
        positive("AGENT_DATABASE_ACQUIRE_TIMEOUT_SECONDS", acquire_seconds)?;
        if min_connections > max_connections {
            return Err(ConfigError::MinConnectionsExceedsMax {
                min: min_connections,
                max: max_connections,
            });
        }
        let max_request_bytes = parse_usize("AGENT_MAX_REQUEST_BYTES", 1024 * 1024)?;
        let outbox_ms = parse_u64("AGENT_NATS_OUTBOX_INTERVAL_MILLISECONDS", 100)?;
        let outbox_batch = parse_i64("AGENT_NATS_OUTBOX_BATCH_SIZE", 100)?;
        let expiry_seconds = parse_u64("AGENT_WAIT_EXPIRY_INTERVAL_SECONDS", 1)?;
        let expiry_batch = parse_i64("AGENT_WAIT_EXPIRY_BATCH_SIZE", 100)?;
        for (name, value) in [
            ("AGENT_MAX_REQUEST_BYTES", max_request_bytes as i128),
            ("AGENT_NATS_OUTBOX_INTERVAL_MILLISECONDS", outbox_ms as i128),
            ("AGENT_NATS_OUTBOX_BATCH_SIZE", outbox_batch as i128),
            ("AGENT_WAIT_EXPIRY_INTERVAL_SECONDS", expiry_seconds as i128),
            ("AGENT_WAIT_EXPIRY_BATCH_SIZE", expiry_batch as i128),
        ] {
            if value <= 0 {
                return Err(ConfigError::NotPositive(name));
            }
        }
        Ok(Self {
            bind_address,
            database: DatabaseConfig {
                url: required("AGENT_DATABASE_URL")?,
                max_connections,
                min_connections,
                acquire_timeout: Duration::from_secs(acquire_seconds),
            },
            nats: NatsConfig {
                url: env::var("AGENT_NATS_URL")
                    .unwrap_or_else(|_| "nats://127.0.0.1:4222".to_owned()),
                outbox_interval: Duration::from_millis(outbox_ms),
                outbox_batch_size: outbox_batch,
                wait_expiry_interval: Duration::from_secs(expiry_seconds),
                wait_expiry_batch_size: expiry_batch,
            },
            control_token: ControlToken::new(required("AGENT_CONTROL_TOKEN")?)?,
            harness_token: HarnessToken::new(required("AGENT_HARNESS_TOKEN")?)?,
            max_request_bytes,
            execution_policy: ExecutionPolicy::new(
                parse_u32("AGENT_DEFAULT_MAX_TURNS", 100)?,
                parse_u32("AGENT_MAX_TURNS", 1_000)?,
                parse_usize("AGENT_HARNESS_MAX_MESSAGE_BATCH", 100)?,
            )?,
        })
    }
}
fn required(name: &'static str) -> Result<String, ConfigError> {
    let value = env::var(name).map_err(|_| ConfigError::Missing(name))?;
    if value.trim().is_empty() {
        Err(ConfigError::Missing(name))
    } else {
        Ok(value)
    }
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
fn parse_i64(name: &'static str, default: i64) -> Result<i64, ConfigError> {
    parse_integer(name, default)
}
fn parse_integer<T>(name: &'static str, default: T) -> Result<T, ConfigError>
where
    T: std::str::FromStr<Err = ParseIntError>,
{
    env::var(name).map_or(Ok(default), |value| {
        value.parse().map_err(|source| ConfigError::InvalidInteger {
            name,
            value,
            source,
        })
    })
}
fn positive<T: PartialOrd + Default>(name: &'static str, value: T) -> Result<(), ConfigError> {
    if value > T::default() {
        Ok(())
    } else {
        Err(ConfigError::NotPositive(name))
    }
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
    HarnessToken(#[from] HarnessTokenError),
    #[error(transparent)]
    ExecutionPolicy(#[from] ExecutionPolicyError),
}
