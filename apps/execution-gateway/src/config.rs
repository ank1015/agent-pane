use std::{net::SocketAddr, str::FromStr, time::Duration};

use credential_vault::CredentialVault;
use thiserror::Error;

#[derive(Clone)]
pub struct Config {
    pub bind_address: SocketAddr,
    pub database_url: String,
    pub database_max_connections: u32,
    pub admin_token: String,
    pub api_token: String,
    pub control_token: String,
    pub vault: CredentialVault,
    pub daemon_websocket_url: String,
    pub registration_ttl: Duration,
    pub max_request_bytes: usize,
}

#[derive(Debug, Error)]
pub enum ConfigError {
    #[error("missing environment variable {0}")]
    Missing(&'static str),
    #[error("invalid environment variable {name}: {message}")]
    Invalid { name: &'static str, message: String },
}

impl Config {
    pub fn from_env() -> Result<Self, ConfigError> {
        Ok(Self {
            bind_address: parse("EXECUTION_GATEWAY_BIND_ADDRESS", "127.0.0.1:8790")?,
            database_url: required("EXECUTION_GATEWAY_DATABASE_URL")?,
            database_max_connections: parse("EXECUTION_GATEWAY_DATABASE_MAX_CONNECTIONS", "10")?,
            admin_token: required("EXECUTION_GATEWAY_ADMIN_TOKEN")?,
            api_token: required("EXECUTION_GATEWAY_API_TOKEN")?,
            control_token: required("EXECUTION_GATEWAY_CONTROL_TOKEN")?,
            vault: CredentialVault::from_base64(&required("EXECUTION_GATEWAY_VAULT_KEY")?)
                .map_err(|source| ConfigError::Invalid {
                    name: "EXECUTION_GATEWAY_VAULT_KEY",
                    message: source.to_string(),
                })?,
            daemon_websocket_url: required("EXECUTION_GATEWAY_DAEMON_WEBSOCKET_URL")?,
            registration_ttl: Duration::from_secs(parse(
                "EXECUTION_GATEWAY_REGISTRATION_TTL_SECONDS",
                "900",
            )?),
            max_request_bytes: parse("EXECUTION_GATEWAY_MAX_REQUEST_BYTES", "1048576")?,
        })
    }
}

fn required(name: &'static str) -> Result<String, ConfigError> {
    std::env::var(name)
        .ok()
        .filter(|value| !value.trim().is_empty())
        .ok_or(ConfigError::Missing(name))
}

fn parse<T>(name: &'static str, default: &str) -> Result<T, ConfigError>
where
    T: FromStr,
    T::Err: std::fmt::Display,
{
    std::env::var(name)
        .unwrap_or_else(|_| default.to_owned())
        .parse()
        .map_err(|source: T::Err| ConfigError::Invalid {
            name,
            message: source.to_string(),
        })
}
