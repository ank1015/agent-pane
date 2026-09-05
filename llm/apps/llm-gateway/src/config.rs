use std::{env, net::SocketAddr, num::ParseIntError, time::Duration};

use zeroize::Zeroizing;

use crate::{
    auth::{AccessToken, AccessTokenError},
    vault::{Vault, VaultError},
};

const DEFAULT_BIND_ADDRESS: &str = "127.0.0.1:3000";
const DEFAULT_MAX_CONNECTIONS: u32 = 10;
const DEFAULT_MIN_CONNECTIONS: u32 = 1;
const DEFAULT_ACQUIRE_TIMEOUT_SECONDS: u64 = 5;
const DEFAULT_MAX_CONCURRENT_REQUESTS: usize = 32;
const DEFAULT_MAX_QUEUED_REQUESTS: usize = 64;
const DEFAULT_QUEUE_TIMEOUT_SECONDS: u64 = 120;
const DEFAULT_MAX_AUTH_FAILURES_PER_MINUTE: u32 = 60;
const DEFAULT_MAX_REQUEST_BYTES: usize = 64 * 1024 * 1024;
const DEFAULT_REQUEST_TIMEOUT_SECONDS: u64 = 35 * 60;
const MAX_REQUEST_BYTES: usize = 256 * 1024 * 1024;
const MAX_REQUEST_TIMEOUT_SECONDS: u64 = 60 * 60;
const DEFAULT_CHATGPT_OAUTH_CLIENT_ID: &str = "app_EMoamEEZ73f0CkXaXp7hrann";
const DEFAULT_CHATGPT_OAUTH_TOKEN_URL: &str = "https://auth.openai.com/oauth/token";

#[derive(Clone)]
pub struct AppConfig {
    pub bind_address: SocketAddr,
    pub database: DatabaseConfig,
    pub vault: Vault,
    pub api_token: AccessToken,
    pub admin_token: AccessToken,
    pub max_concurrent_requests: usize,
    pub max_queued_requests: usize,
    pub queue_timeout: Duration,
    pub max_authentication_failures_per_minute: u32,
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

impl DatabaseConfig {
    /// Creates a database configuration with conservative service defaults.
    #[must_use]
    pub fn new(url: impl Into<String>) -> Self {
        Self {
            url: url.into(),
            max_connections: DEFAULT_MAX_CONNECTIONS,
            min_connections: DEFAULT_MIN_CONNECTIONS,
            acquire_timeout: Duration::from_secs(DEFAULT_ACQUIRE_TIMEOUT_SECONDS),
        }
    }
}

impl AppConfig {
    pub fn from_env() -> Result<Self, ConfigError> {
        let database_url = required("LLM_GATEWAY_DATABASE_URL")?;
        let max_connections = parse_u32(
            "LLM_GATEWAY_DATABASE_MAX_CONNECTIONS",
            DEFAULT_MAX_CONNECTIONS,
        )?;
        let min_connections = parse_u32(
            "LLM_GATEWAY_DATABASE_MIN_CONNECTIONS",
            DEFAULT_MIN_CONNECTIONS,
        )?;
        require_positive("LLM_GATEWAY_DATABASE_MAX_CONNECTIONS", max_connections)?;
        if min_connections > max_connections {
            return Err(ConfigError::MinConnectionsExceedsMax {
                min: min_connections,
                max: max_connections,
            });
        }
        let acquire_timeout_seconds = parse_u64(
            "LLM_GATEWAY_DATABASE_ACQUIRE_TIMEOUT_SECONDS",
            DEFAULT_ACQUIRE_TIMEOUT_SECONDS,
        )?;
        require_positive(
            "LLM_GATEWAY_DATABASE_ACQUIRE_TIMEOUT_SECONDS",
            acquire_timeout_seconds,
        )?;
        let max_concurrent_requests = parse_usize(
            "LLM_GATEWAY_MAX_CONCURRENT_REQUESTS",
            DEFAULT_MAX_CONCURRENT_REQUESTS,
        )?;
        require_positive(
            "LLM_GATEWAY_MAX_CONCURRENT_REQUESTS",
            max_concurrent_requests,
        )?;
        let max_queued_requests = parse_usize(
            "LLM_GATEWAY_MAX_QUEUED_REQUESTS",
            DEFAULT_MAX_QUEUED_REQUESTS,
        )?;
        require_positive("LLM_GATEWAY_MAX_QUEUED_REQUESTS", max_queued_requests)?;
        let queue_timeout_seconds = parse_u64(
            "LLM_GATEWAY_QUEUE_TIMEOUT_SECONDS",
            DEFAULT_QUEUE_TIMEOUT_SECONDS,
        )?;
        require_positive("LLM_GATEWAY_QUEUE_TIMEOUT_SECONDS", queue_timeout_seconds)?;
        let max_authentication_failures_per_minute = parse_u32(
            "LLM_GATEWAY_MAX_AUTH_FAILURES_PER_MINUTE",
            DEFAULT_MAX_AUTH_FAILURES_PER_MINUTE,
        )?;
        require_positive(
            "LLM_GATEWAY_MAX_AUTH_FAILURES_PER_MINUTE",
            max_authentication_failures_per_minute,
        )?;
        let max_request_bytes =
            parse_usize("LLM_GATEWAY_MAX_REQUEST_BYTES", DEFAULT_MAX_REQUEST_BYTES)?;
        require_positive("LLM_GATEWAY_MAX_REQUEST_BYTES", max_request_bytes)?;
        if max_request_bytes > MAX_REQUEST_BYTES {
            return Err(ConfigError::ValueTooLarge {
                name: "LLM_GATEWAY_MAX_REQUEST_BYTES",
                maximum: MAX_REQUEST_BYTES as u64,
            });
        }
        let request_timeout_seconds = parse_u64(
            "LLM_GATEWAY_REQUEST_TIMEOUT_SECONDS",
            DEFAULT_REQUEST_TIMEOUT_SECONDS,
        )?;
        require_positive(
            "LLM_GATEWAY_REQUEST_TIMEOUT_SECONDS",
            request_timeout_seconds,
        )?;
        if request_timeout_seconds > MAX_REQUEST_TIMEOUT_SECONDS {
            return Err(ConfigError::ValueTooLarge {
                name: "LLM_GATEWAY_REQUEST_TIMEOUT_SECONDS",
                maximum: MAX_REQUEST_TIMEOUT_SECONDS,
            });
        }

        let bind_address = env::var("LLM_GATEWAY_BIND_ADDRESS")
            .unwrap_or_else(|_| DEFAULT_BIND_ADDRESS.to_owned())
            .parse()
            .map_err(ConfigError::InvalidBindAddress)?;
        let vault_key = Zeroizing::new(required("LLM_GATEWAY_VAULT_KEY")?);
        let vault = Vault::from_base64(&vault_key)?;
        let api_token_value = Zeroizing::new(required("LLM_GATEWAY_API_TOKEN")?);
        let admin_token_value = Zeroizing::new(required("LLM_GATEWAY_ADMIN_TOKEN")?);
        if api_token_value.as_bytes() == admin_token_value.as_bytes() {
            return Err(ConfigError::TokensMustDiffer);
        }
        let api_token = AccessToken::new(api_token_value.as_bytes())?;
        let admin_token = AccessToken::new(admin_token_value.as_bytes())?;
        let chatgpt_oauth_client_id = env::var("LLM_GATEWAY_CHATGPT_OAUTH_CLIENT_ID")
            .unwrap_or_else(|_| DEFAULT_CHATGPT_OAUTH_CLIENT_ID.to_owned());
        if chatgpt_oauth_client_id.trim().is_empty() {
            return Err(ConfigError::Missing("LLM_GATEWAY_CHATGPT_OAUTH_CLIENT_ID"));
        }
        let chatgpt_oauth_token_url = env::var("LLM_GATEWAY_CHATGPT_OAUTH_TOKEN_URL")
            .unwrap_or_else(|_| DEFAULT_CHATGPT_OAUTH_TOKEN_URL.to_owned());
        validate_sensitive_url(&chatgpt_oauth_token_url)?;

        Ok(Self {
            bind_address,
            database: DatabaseConfig {
                max_connections,
                min_connections,
                acquire_timeout: Duration::from_secs(acquire_timeout_seconds),
                ..DatabaseConfig::new(database_url)
            },
            vault,
            api_token,
            admin_token,
            max_concurrent_requests,
            max_queued_requests,
            queue_timeout: Duration::from_secs(queue_timeout_seconds),
            max_authentication_failures_per_minute,
            max_request_bytes,
            request_timeout: Duration::from_secs(request_timeout_seconds),
            chatgpt_oauth_client_id,
            chatgpt_oauth_token_url,
        })
    }
}

fn validate_sensitive_url(value: &str) -> Result<(), ConfigError> {
    let parsed = url::Url::parse(value).map_err(ConfigError::InvalidChatGptOauthTokenUrl)?;
    let loopback_http = parsed.scheme() == "http"
        && parsed.host().is_some_and(|host| match host {
            url::Host::Domain(domain) => domain.eq_ignore_ascii_case("localhost"),
            url::Host::Ipv4(address) => address.is_loopback(),
            url::Host::Ipv6(address) => address.is_loopback(),
        });
    if !parsed.has_host()
        || (parsed.scheme() != "https" && !loopback_http)
        || !parsed.username().is_empty()
        || parsed.password().is_some()
        || parsed.query().is_some()
        || parsed.fragment().is_some()
    {
        return Err(ConfigError::InsecureChatGptOauthTokenUrl);
    }
    Ok(())
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
    #[error("LLM_GATEWAY_BIND_ADDRESS is not a valid socket address")]
    InvalidBindAddress(#[source] std::net::AddrParseError),
    #[error("{0} must be greater than zero")]
    NotPositive(&'static str),
    #[error("{name} must not exceed {maximum}")]
    ValueTooLarge { name: &'static str, maximum: u64 },
    #[error("minimum database connections ({min}) cannot exceed maximum ({max})")]
    MinConnectionsExceedsMax { min: u32, max: u32 },
    #[error(transparent)]
    Vault(#[from] VaultError),
    #[error(transparent)]
    AccessToken(#[from] AccessTokenError),
    #[error("LLM_GATEWAY_API_TOKEN and LLM_GATEWAY_ADMIN_TOKEN must differ")]
    TokensMustDiffer,
    #[error("LLM_GATEWAY_CHATGPT_OAUTH_TOKEN_URL is not a valid URL")]
    InvalidChatGptOauthTokenUrl(#[source] url::ParseError),
    #[error(
        "LLM_GATEWAY_CHATGPT_OAUTH_TOKEN_URL must use HTTPS (loopback HTTP is allowed for local testing) and must not contain credentials, a query, or a fragment"
    )]
    InsecureChatGptOauthTokenUrl,
}

#[cfg(test)]
mod tests {
    use super::validate_sensitive_url;

    #[test]
    fn sensitive_urls_require_https_except_on_loopback() {
        assert!(validate_sensitive_url("https://auth.openai.com/oauth/token").is_ok());
        assert!(validate_sensitive_url("http://127.0.0.1:8080/token").is_ok());
        assert!(validate_sensitive_url("http://localhost:8080/token").is_ok());
        assert!(validate_sensitive_url("http://auth.example.com/token").is_err());
        assert!(validate_sensitive_url("https://user:password@example.com/token").is_err());
        assert!(validate_sensitive_url("https://example.com/token?secret=value").is_err());
    }
}
