use std::{env, net::SocketAddr, num::ParseIntError, time::Duration};

use zeroize::Zeroizing;

use crate::{
    auth::{AdminToken, AdminTokenError},
    vault::{Vault, VaultError},
};

const DEFAULT_BIND_ADDRESS: &str = "127.0.0.1:3000";
const DEFAULT_MAX_CONNECTIONS: u32 = 10;
const DEFAULT_MIN_CONNECTIONS: u32 = 1;
const DEFAULT_ACQUIRE_TIMEOUT_SECONDS: u64 = 5;
const DEFAULT_MAX_CONCURRENT_REQUESTS: usize = 128;
const DEFAULT_MAX_REQUEST_BYTES: usize = 64 * 1024 * 1024;
const DEFAULT_REQUEST_TIMEOUT_SECONDS: u64 = 35 * 60;
const DEFAULT_CHATGPT_OAUTH_CLIENT_ID: &str = "app_EMoamEEZ73f0CkXaXp7hrann";
const DEFAULT_CHATGPT_OAUTH_TOKEN_URL: &str = "https://auth.openai.com/oauth/token";

#[derive(Clone)]
pub struct AppConfig {
    pub bind_address: SocketAddr,
    pub database: DatabaseConfig,
    pub vault: Vault,
    pub admin_token: AdminToken,
    pub max_concurrent_requests: usize,
    pub max_request_bytes: usize,
    pub request_timeout: Duration,
    pub chatgpt_oauth_client_id: String,
    pub chatgpt_oauth_token_url: String,
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
        let database_url = required("DATABASE_URL")?;
        let max_connections = parse_u32("DATABASE_MAX_CONNECTIONS", DEFAULT_MAX_CONNECTIONS)?;
        let min_connections = parse_u32("DATABASE_MIN_CONNECTIONS", DEFAULT_MIN_CONNECTIONS)?;
        if max_connections == 0 {
            return Err(ConfigError::NotPositive("DATABASE_MAX_CONNECTIONS"));
        }
        if min_connections > max_connections {
            return Err(ConfigError::MinConnectionsExceedsMax {
                min: min_connections,
                max: max_connections,
            });
        }

        let acquire_timeout_seconds = parse_u64(
            "DATABASE_ACQUIRE_TIMEOUT_SECONDS",
            DEFAULT_ACQUIRE_TIMEOUT_SECONDS,
        )?;
        require_positive("DATABASE_ACQUIRE_TIMEOUT_SECONDS", acquire_timeout_seconds)?;

        let max_concurrent_requests = parse_usize(
            "GATEWAY_MAX_CONCURRENT_REQUESTS",
            DEFAULT_MAX_CONCURRENT_REQUESTS,
        )?;
        require_positive("GATEWAY_MAX_CONCURRENT_REQUESTS", max_concurrent_requests)?;

        let max_request_bytes =
            parse_usize("GATEWAY_MAX_REQUEST_BYTES", DEFAULT_MAX_REQUEST_BYTES)?;
        require_positive("GATEWAY_MAX_REQUEST_BYTES", max_request_bytes)?;

        let request_timeout_seconds = parse_u64(
            "GATEWAY_REQUEST_TIMEOUT_SECONDS",
            DEFAULT_REQUEST_TIMEOUT_SECONDS,
        )?;
        require_positive("GATEWAY_REQUEST_TIMEOUT_SECONDS", request_timeout_seconds)?;

        let bind_address = env::var("GATEWAY_BIND_ADDRESS")
            .unwrap_or_else(|_| DEFAULT_BIND_ADDRESS.to_owned())
            .parse()
            .map_err(ConfigError::InvalidBindAddress)?;
        let master_key = Zeroizing::new(required("GATEWAY_MASTER_KEY")?);
        let vault = Vault::from_base64(&master_key)?;
        let admin_token_value = Zeroizing::new(required("GATEWAY_ADMIN_TOKEN")?);
        let admin_token = AdminToken::new(admin_token_value.as_bytes())?;
        let chatgpt_oauth_client_id = env::var("CHATGPT_OAUTH_CLIENT_ID")
            .unwrap_or_else(|_| DEFAULT_CHATGPT_OAUTH_CLIENT_ID.to_owned());
        if chatgpt_oauth_client_id.trim().is_empty() {
            return Err(ConfigError::Missing("CHATGPT_OAUTH_CLIENT_ID"));
        }
        let chatgpt_oauth_token_url = env::var("CHATGPT_OAUTH_TOKEN_URL")
            .unwrap_or_else(|_| DEFAULT_CHATGPT_OAUTH_TOKEN_URL.to_owned());
        let parsed_token_url = url::Url::parse(&chatgpt_oauth_token_url)
            .map_err(ConfigError::InvalidChatGptOauthTokenUrl)?;
        if !matches!(parsed_token_url.scheme(), "http" | "https") {
            return Err(ConfigError::UnsupportedChatGptOauthTokenUrlScheme(
                parsed_token_url.scheme().to_owned(),
            ));
        }

        Ok(Self {
            bind_address,
            database: DatabaseConfig {
                url: database_url,
                max_connections,
                min_connections,
                acquire_timeout: Duration::from_secs(acquire_timeout_seconds),
            },
            vault,
            admin_token,
            max_concurrent_requests,
            max_request_bytes,
            request_timeout: Duration::from_secs(request_timeout_seconds),
            chatgpt_oauth_client_id,
            chatgpt_oauth_token_url,
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

fn require_positive<T>(name: &'static str, value: T) -> Result<(), ConfigError>
where
    T: PartialEq + From<u8>,
{
    if value == T::from(0) {
        return Err(ConfigError::NotPositive(name));
    }
    Ok(())
}

#[derive(Debug, thiserror::Error)]
pub enum ConfigError {
    #[error("required environment variable {0} is missing or empty")]
    Missing(&'static str),
    #[error("{name} must be an integer, got {value:?}")]
    InvalidInteger {
        name: &'static str,
        value: String,
        #[source]
        source: ParseIntError,
    },
    #[error("GATEWAY_BIND_ADDRESS is not a valid socket address")]
    InvalidBindAddress(#[source] std::net::AddrParseError),
    #[error("{0} must be greater than zero")]
    NotPositive(&'static str),
    #[error("DATABASE_MIN_CONNECTIONS ({min}) cannot exceed DATABASE_MAX_CONNECTIONS ({max})")]
    MinConnectionsExceedsMax { min: u32, max: u32 },
    #[error(transparent)]
    Vault(#[from] VaultError),
    #[error(transparent)]
    AdminToken(#[from] AdminTokenError),
    #[error("CHATGPT_OAUTH_TOKEN_URL is not a valid URL")]
    InvalidChatGptOauthTokenUrl(#[source] url::ParseError),
    #[error("CHATGPT_OAUTH_TOKEN_URL must use http or https, not {0:?}")]
    UnsupportedChatGptOauthTokenUrlScheme(String),
}
