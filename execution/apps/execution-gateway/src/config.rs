use std::{net::SocketAddr, str::FromStr, time::Duration};

use thiserror::Error;
use url::Url;

use crate::crypto::CredentialVault;

#[derive(Clone)]
pub struct Config {
    pub bind_address: SocketAddr,
    pub database_url: String,
    pub database_max_connections: u32,
    pub api_token: String,
    pub vault: CredentialVault,
    pub default_host_timeout_seconds: u64,
    pub public_base_url: Url,
    pub registration_token_ttl: Duration,
    pub registered_heartbeat_interval: Duration,
    pub registered_operation_timeout: Duration,
    pub registered_max_in_flight: usize,
    pub max_operations_in_flight: usize,
    pub max_authentication_failures_per_minute: u32,
    pub max_machine_entry_attempts_per_minute: u32,
    pub e2b_control_base_url: Url,
    pub e2b_envd_base_url_override: Option<Url>,
    pub e2b_request_timeout: Duration,
    pub reconcile_interval: Duration,
    pub max_request_bytes: usize,
    pub allow_insecure_http: bool,
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
        let bind_address = bind_address()?;
        let default_host_timeout_seconds =
            parse("EXECUTION_GATEWAY_DEFAULT_HOST_TIMEOUT_SECONDS", "3600")?;
        if !(1..=86_400).contains(&default_host_timeout_seconds) {
            return Err(ConfigError::Invalid {
                name: "EXECUTION_GATEWAY_DEFAULT_HOST_TIMEOUT_SECONDS",
                message: "must be between 1 and 86400".to_owned(),
            });
        }
        let public_base_url = parse_url(
            "EXECUTION_GATEWAY_PUBLIC_URL",
            &format!("http://{bind_address}/"),
        )?;
        if !matches!(public_base_url.scheme(), "http" | "https" | "ws" | "wss") {
            return Err(ConfigError::Invalid {
                name: "EXECUTION_GATEWAY_PUBLIC_URL",
                message: "must use http, https, ws, or wss".to_owned(),
            });
        }
        let allow_insecure_http: bool = parse("EXECUTION_GATEWAY_ALLOW_INSECURE_HTTP", "false")?;
        if matches!(public_base_url.scheme(), "http" | "ws") && !allow_insecure_http {
            return Err(ConfigError::Invalid {
                name: "EXECUTION_GATEWAY_PUBLIC_URL",
                message: "must use https/wss unless EXECUTION_GATEWAY_ALLOW_INSECURE_HTTP=true"
                    .to_owned(),
            });
        }
        let registration_token_ttl = Duration::from_secs(positive(
            "EXECUTION_GATEWAY_REGISTRATION_TOKEN_TTL_SECONDS",
            "900",
        )?);
        let registered_heartbeat_interval = Duration::from_secs(positive(
            "EXECUTION_GATEWAY_REGISTERED_HEARTBEAT_SECONDS",
            "15",
        )?);
        let registered_operation_timeout = Duration::from_secs(positive(
            "EXECUTION_GATEWAY_REGISTERED_OPERATION_TIMEOUT_SECONDS",
            "60",
        )?);
        let registered_max_in_flight =
            positive("EXECUTION_GATEWAY_REGISTERED_MAX_IN_FLIGHT", "64")?;
        let max_operations_in_flight =
            positive("EXECUTION_GATEWAY_MAX_OPERATIONS_IN_FLIGHT", "32")?;
        let max_authentication_failures_per_minute =
            positive("EXECUTION_GATEWAY_MAX_AUTH_FAILURES_PER_MINUTE", "30")?;
        let max_machine_entry_attempts_per_minute = positive(
            "EXECUTION_GATEWAY_MAX_MACHINE_ENTRY_ATTEMPTS_PER_MINUTE",
            "60",
        )?;
        let api_token = required("EXECUTION_GATEWAY_API_TOKEN")?;
        if api_token.trim() != api_token || !(32..=512).contains(&api_token.len()) {
            return Err(ConfigError::Invalid {
                name: "EXECUTION_GATEWAY_API_TOKEN",
                message: "must be 32 to 512 characters without surrounding whitespace".to_owned(),
            });
        }

        Ok(Self {
            bind_address,
            database_url: required("EXECUTION_GATEWAY_DATABASE_URL")?,
            database_max_connections: parse("EXECUTION_GATEWAY_DATABASE_MAX_CONNECTIONS", "10")?,
            api_token,
            vault: CredentialVault::from_base64(&required("EXECUTION_GATEWAY_VAULT_KEY")?)
                .map_err(|error| ConfigError::Invalid {
                    name: "EXECUTION_GATEWAY_VAULT_KEY",
                    message: error.to_string(),
                })?,
            default_host_timeout_seconds,
            public_base_url,
            registration_token_ttl,
            registered_heartbeat_interval,
            registered_operation_timeout,
            registered_max_in_flight,
            max_operations_in_flight,
            max_authentication_failures_per_minute,
            max_machine_entry_attempts_per_minute,
            e2b_control_base_url: parse_url(
                "EXECUTION_GATEWAY_E2B_CONTROL_BASE_URL",
                "https://api.e2b.app/",
            )?,
            e2b_envd_base_url_override: optional_url("EXECUTION_GATEWAY_E2B_ENVD_BASE_URL")?,
            e2b_request_timeout: Duration::from_secs(parse(
                "EXECUTION_GATEWAY_E2B_REQUEST_TIMEOUT_SECONDS",
                "60",
            )?),
            reconcile_interval: Duration::from_millis(parse(
                "EXECUTION_GATEWAY_RECONCILE_INTERVAL_MS",
                "1000",
            )?),
            max_request_bytes: parse("EXECUTION_GATEWAY_MAX_REQUEST_BYTES", "16777216")?,
            allow_insecure_http,
        })
    }
}

fn bind_address() -> Result<SocketAddr, ConfigError> {
    if std::env::var_os("EXECUTION_GATEWAY_BIND_ADDRESS").is_some() {
        return parse("EXECUTION_GATEWAY_BIND_ADDRESS", "127.0.0.1:8790");
    }
    if let Ok(port) = std::env::var("PORT") {
        return format!("0.0.0.0:{port}")
            .parse()
            .map_err(|error: std::net::AddrParseError| ConfigError::Invalid {
                name: "PORT",
                message: error.to_string(),
            });
    }
    "127.0.0.1:8790"
        .parse()
        .map_err(|error: std::net::AddrParseError| ConfigError::Invalid {
            name: "EXECUTION_GATEWAY_BIND_ADDRESS",
            message: error.to_string(),
        })
}

fn positive<T>(name: &'static str, default: &str) -> Result<T, ConfigError>
where
    T: FromStr + PartialOrd + From<u8>,
    T::Err: std::fmt::Display,
{
    let value = parse(name, default)?;
    if value <= T::from(0) {
        return Err(ConfigError::Invalid {
            name,
            message: "must be greater than zero".to_owned(),
        });
    }
    Ok(value)
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
        .parse::<T>()
        .map_err(|error| ConfigError::Invalid {
            name,
            message: error.to_string(),
        })
}

fn parse_url(name: &'static str, default: &str) -> Result<Url, ConfigError> {
    let value = std::env::var(name).unwrap_or_else(|_| default.to_owned());
    Url::parse(&value).map_err(|error: url::ParseError| ConfigError::Invalid {
        name,
        message: error.to_string(),
    })
}

fn optional_url(name: &'static str) -> Result<Option<Url>, ConfigError> {
    std::env::var(name)
        .ok()
        .filter(|value| !value.trim().is_empty())
        .map(|value| {
            Url::parse(&value).map_err(|error| ConfigError::Invalid {
                name,
                message: error.to_string(),
            })
        })
        .transpose()
}
