use std::{env, net::SocketAddr, num::ParseIntError, time::Duration};

use thiserror::Error;
use url::Url;

const DEFAULT_BIND_ADDRESS: &str = "127.0.0.1:3100";
const DEFAULT_EXECUTION_GATEWAY_TIMEOUT_SECONDS: u64 = 30;
const DEFAULT_DATABASE_MIN_CONNECTIONS: u32 = 1;
const DEFAULT_DATABASE_MAX_CONNECTIONS: u32 = 10;
const DEFAULT_DATABASE_ACQUIRE_TIMEOUT_SECONDS: u64 = 5;

pub struct AppConfig {
    pub bind_address: SocketAddr,
    pub database_url: zeroize::Zeroizing<String>,
    pub database_min_connections: u32,
    pub database_max_connections: u32,
    pub database_acquire_timeout: Duration,
    pub execution_gateway_url: Url,
    pub execution_gateway_token: String,
    pub execution_gateway_timeout: Duration,
    pub llm_gateway_url: Url,
    pub llm_gateway_admin_token: zeroize::Zeroizing<String>,
    pub llm_gateway_timeout: Duration,
}

impl AppConfig {
    pub fn from_env() -> Result<Self, ConfigError> {
        let bind_address = env::var("PLATFORM_SERVER_BIND_ADDRESS")
            .unwrap_or_else(|_| DEFAULT_BIND_ADDRESS.to_owned())
            .parse()
            .map_err(ConfigError::InvalidBindAddress)?;

        let database_url = zeroize::Zeroizing::new(required("PLATFORM_SERVER_DATABASE_URL")?);
        let database_min_connections: u32 = env::var("PLATFORM_SERVER_DATABASE_MIN_CONNECTIONS")
            .unwrap_or_else(|_| DEFAULT_DATABASE_MIN_CONNECTIONS.to_string())
            .parse()
            .map_err(ConfigError::InvalidDatabaseMinConnections)?;
        let database_max_connections: u32 = env::var("PLATFORM_SERVER_DATABASE_MAX_CONNECTIONS")
            .unwrap_or_else(|_| DEFAULT_DATABASE_MAX_CONNECTIONS.to_string())
            .parse()
            .map_err(ConfigError::InvalidDatabaseMaxConnections)?;
        let database_acquire_timeout_seconds: u64 =
            env::var("PLATFORM_SERVER_DATABASE_ACQUIRE_TIMEOUT_SECONDS")
                .unwrap_or_else(|_| DEFAULT_DATABASE_ACQUIRE_TIMEOUT_SECONDS.to_string())
                .parse()
                .map_err(ConfigError::InvalidDatabaseAcquireTimeout)?;
        if database_max_connections == 0 || database_acquire_timeout_seconds == 0 {
            return Err(ConfigError::InvalidDatabasePoolLimits);
        }
        if database_min_connections > database_max_connections {
            return Err(ConfigError::DatabaseMinExceedsMax);
        }

        let execution_gateway_url = env::var("PLATFORM_SERVER_EXECUTION_GATEWAY_URL")
            .map_err(|_| ConfigError::MissingExecutionGatewayUrl)
            .and_then(|value| {
                Url::parse(&value).map_err(ConfigError::InvalidExecutionGatewayUrl)
            })?;
        if !matches!(execution_gateway_url.scheme(), "http" | "https") {
            return Err(ConfigError::UnsupportedExecutionGatewayUrlScheme(
                execution_gateway_url.scheme().to_owned(),
            ));
        }

        let execution_gateway_token = required("PLATFORM_SERVER_EXECUTION_GATEWAY_TOKEN")?;
        let timeout_seconds = env::var("PLATFORM_SERVER_EXECUTION_GATEWAY_TIMEOUT_SECONDS")
            .unwrap_or_else(|_| DEFAULT_EXECUTION_GATEWAY_TIMEOUT_SECONDS.to_string())
            .parse()
            .map_err(ConfigError::InvalidExecutionGatewayTimeout)?;
        if timeout_seconds == 0 {
            return Err(ConfigError::ZeroExecutionGatewayTimeout);
        }

        let llm_gateway_url = Url::parse(&required("PLATFORM_SERVER_LLM_GATEWAY_URL")?)
            .map_err(ConfigError::InvalidLlmGatewayUrl)?;
        let llm_gateway_admin_token =
            zeroize::Zeroizing::new(required("PLATFORM_SERVER_LLM_GATEWAY_ADMIN_TOKEN")?);
        let llm_timeout_seconds: u64 = env::var("PLATFORM_SERVER_LLM_GATEWAY_TIMEOUT_SECONDS")
            .unwrap_or_else(|_| "30".to_owned())
            .parse()
            .map_err(ConfigError::InvalidLlmGatewayTimeout)?;
        if llm_timeout_seconds == 0 {
            return Err(ConfigError::ZeroLlmGatewayTimeout);
        }

        Ok(Self {
            bind_address,
            database_url,
            database_min_connections,
            database_max_connections,
            database_acquire_timeout: Duration::from_secs(database_acquire_timeout_seconds),
            execution_gateway_url,
            execution_gateway_token,
            execution_gateway_timeout: Duration::from_secs(timeout_seconds),
            llm_gateway_url,
            llm_gateway_admin_token,
            llm_gateway_timeout: Duration::from_secs(llm_timeout_seconds),
        })
    }
}

fn required(name: &'static str) -> Result<String, ConfigError> {
    let value = env::var(name).map_err(|_| ConfigError::MissingVariable(name))?;
    if value.trim().is_empty() || value.trim() != value {
        return Err(ConfigError::InvalidVariable(name));
    }
    Ok(value)
}

#[derive(Debug, Error)]
pub enum ConfigError {
    #[error("PLATFORM_SERVER_DATABASE_MIN_CONNECTIONS must be an integer")]
    InvalidDatabaseMinConnections(#[source] ParseIntError),
    #[error("PLATFORM_SERVER_DATABASE_MAX_CONNECTIONS must be an integer")]
    InvalidDatabaseMaxConnections(#[source] ParseIntError),
    #[error("PLATFORM_SERVER_DATABASE_ACQUIRE_TIMEOUT_SECONDS must be an integer")]
    InvalidDatabaseAcquireTimeout(#[source] ParseIntError),
    #[error("database max connections and acquire timeout must be positive")]
    InvalidDatabasePoolLimits,
    #[error("database min connections cannot exceed max connections")]
    DatabaseMinExceedsMax,
    #[error("PLATFORM_SERVER_LLM_GATEWAY_URL is not a valid URL")]
    InvalidLlmGatewayUrl(#[source] url::ParseError),
    #[error("PLATFORM_SERVER_LLM_GATEWAY_TIMEOUT_SECONDS must be an integer")]
    InvalidLlmGatewayTimeout(#[source] ParseIntError),
    #[error("PLATFORM_SERVER_LLM_GATEWAY_TIMEOUT_SECONDS must be positive")]
    ZeroLlmGatewayTimeout,
    #[error("PLATFORM_SERVER_BIND_ADDRESS is not a valid socket address")]
    InvalidBindAddress(#[source] std::net::AddrParseError),
    #[error("PLATFORM_SERVER_EXECUTION_GATEWAY_URL is required")]
    MissingExecutionGatewayUrl,
    #[error("PLATFORM_SERVER_EXECUTION_GATEWAY_URL is not a valid URL")]
    InvalidExecutionGatewayUrl(#[source] url::ParseError),
    #[error("PLATFORM_SERVER_EXECUTION_GATEWAY_URL must use http or https, not {0}")]
    UnsupportedExecutionGatewayUrlScheme(String),
    #[error("{0} is required")]
    MissingVariable(&'static str),
    #[error("{0} must be non-empty and have no surrounding whitespace")]
    InvalidVariable(&'static str),
    #[error("PLATFORM_SERVER_EXECUTION_GATEWAY_TIMEOUT_SECONDS must be an integer")]
    InvalidExecutionGatewayTimeout(#[source] ParseIntError),
    #[error("PLATFORM_SERVER_EXECUTION_GATEWAY_TIMEOUT_SECONDS must be positive")]
    ZeroExecutionGatewayTimeout,
}
