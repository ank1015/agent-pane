use std::{env, net::SocketAddr, num::ParseIntError, time::Duration};

use url::Url;
use zeroize::Zeroizing;

const DEFAULT_BIND_ADDRESS: &str = "127.0.0.1:3100";
const DEFAULT_MAX_REQUEST_BYTES: usize = 1024 * 1024;
const DEFAULT_LLM_GATEWAY_URL: &str = "http://127.0.0.1:3000";
const DEFAULT_LLM_GATEWAY_TIMEOUT_SECONDS: u64 = 30;
const DEFAULT_EXECUTION_GATEWAY_URL: &str = "http://127.0.0.1:8790";
const DEFAULT_EXECUTION_GATEWAY_TIMEOUT_SECONDS: u64 = 30;
const DEFAULT_CHATGPT_OAUTH_CALLBACK_ADDRESS: &str = "127.0.0.1:1455";
const DEFAULT_CHATGPT_OAUTH_CLIENT_ID: &str = "app_EMoamEEZ73f0CkXaXp7hrann";
const DEFAULT_CHATGPT_OAUTH_ISSUER: &str = "https://auth.openai.com";
const DEFAULT_DASHBOARD_ORIGIN: &str = "http://localhost:5173";

pub struct AppConfig {
    pub bind_address: SocketAddr,
    pub max_request_bytes: usize,
    pub llm_gateway_url: Url,
    pub llm_gateway_admin_token: Zeroizing<String>,
    pub llm_gateway_timeout: Duration,
    pub execution_gateway_url: Url,
    pub execution_gateway_control_token: Zeroizing<String>,
    pub execution_gateway_timeout: Duration,
    pub chatgpt_oauth_callback_address: SocketAddr,
    pub chatgpt_oauth_client_id: String,
    pub chatgpt_oauth_issuer: Url,
    pub chatgpt_oauth_redirect_uri: Url,
    pub dashboard_origin: Url,
}

impl AppConfig {
    pub fn from_env() -> Result<Self, ConfigError> {
        let bind_address = env::var("PLATFORM_BIND_ADDRESS")
            .unwrap_or_else(|_| DEFAULT_BIND_ADDRESS.to_owned())
            .parse()
            .map_err(ConfigError::InvalidBindAddress)?;

        let max_request_bytes =
            parse_integer("PLATFORM_MAX_REQUEST_BYTES", DEFAULT_MAX_REQUEST_BYTES)?;
        require_positive("PLATFORM_MAX_REQUEST_BYTES", max_request_bytes)?;

        let llm_gateway_url_value = env::var("PLATFORM_LLM_GATEWAY_URL")
            .unwrap_or_else(|_| DEFAULT_LLM_GATEWAY_URL.to_owned());
        let llm_gateway_url =
            Url::parse(&llm_gateway_url_value).map_err(ConfigError::InvalidLlmGatewayUrl)?;
        if !matches!(llm_gateway_url.scheme(), "http" | "https") {
            return Err(ConfigError::UnsupportedLlmGatewayUrlScheme(
                llm_gateway_url.scheme().to_owned(),
            ));
        }

        let llm_gateway_admin_token = Zeroizing::new(
            env::var("PLATFORM_LLM_GATEWAY_ADMIN_TOKEN")
                .or_else(|_| env::var("GATEWAY_ADMIN_TOKEN"))
                .map_err(|_| ConfigError::MissingLlmGatewayAdminToken)
                .and_then(|value| {
                    if value.trim().is_empty() {
                        Err(ConfigError::MissingLlmGatewayAdminToken)
                    } else {
                        Ok(value)
                    }
                })?,
        );
        let timeout_seconds = parse_integer(
            "PLATFORM_LLM_GATEWAY_TIMEOUT_SECONDS",
            DEFAULT_LLM_GATEWAY_TIMEOUT_SECONDS,
        )?;
        require_positive("PLATFORM_LLM_GATEWAY_TIMEOUT_SECONDS", timeout_seconds)?;

        let execution_gateway_url = parse_http_url(
            "PLATFORM_EXECUTION_GATEWAY_URL",
            &env::var("PLATFORM_EXECUTION_GATEWAY_URL")
                .unwrap_or_else(|_| DEFAULT_EXECUTION_GATEWAY_URL.to_owned()),
        )?;
        let execution_gateway_control_token = Zeroizing::new(
            env::var("PLATFORM_EXECUTION_GATEWAY_CONTROL_TOKEN")
                .or_else(|_| env::var("EXECUTION_GATEWAY_CONTROL_TOKEN"))
                .map_err(|_| ConfigError::MissingExecutionGatewayControlToken)
                .and_then(|value| {
                    if value.trim().is_empty() {
                        Err(ConfigError::MissingExecutionGatewayControlToken)
                    } else {
                        Ok(value)
                    }
                })?,
        );
        let execution_gateway_timeout_seconds = parse_integer(
            "PLATFORM_EXECUTION_GATEWAY_TIMEOUT_SECONDS",
            DEFAULT_EXECUTION_GATEWAY_TIMEOUT_SECONDS,
        )?;
        require_positive(
            "PLATFORM_EXECUTION_GATEWAY_TIMEOUT_SECONDS",
            execution_gateway_timeout_seconds,
        )?;

        let chatgpt_oauth_callback_address: SocketAddr =
            env::var("PLATFORM_CHATGPT_OAUTH_CALLBACK_ADDRESS")
                .unwrap_or_else(|_| DEFAULT_CHATGPT_OAUTH_CALLBACK_ADDRESS.to_owned())
                .parse()
                .map_err(ConfigError::InvalidChatGptCallbackAddress)?;
        let chatgpt_oauth_client_id = env::var("PLATFORM_CHATGPT_OAUTH_CLIENT_ID")
            .unwrap_or_else(|_| DEFAULT_CHATGPT_OAUTH_CLIENT_ID.to_owned());
        let chatgpt_oauth_issuer = parse_http_url(
            "PLATFORM_CHATGPT_OAUTH_ISSUER",
            &env::var("PLATFORM_CHATGPT_OAUTH_ISSUER")
                .unwrap_or_else(|_| DEFAULT_CHATGPT_OAUTH_ISSUER.to_owned()),
        )?;
        let default_redirect_uri = format!(
            "http://localhost:{}/auth/callback",
            chatgpt_oauth_callback_address.port()
        );
        let chatgpt_oauth_redirect_uri = parse_http_url(
            "PLATFORM_CHATGPT_OAUTH_REDIRECT_URI",
            &env::var("PLATFORM_CHATGPT_OAUTH_REDIRECT_URI").unwrap_or(default_redirect_uri),
        )?;
        let dashboard_origin = parse_http_url(
            "PLATFORM_DASHBOARD_ORIGIN",
            &env::var("PLATFORM_DASHBOARD_ORIGIN")
                .unwrap_or_else(|_| DEFAULT_DASHBOARD_ORIGIN.to_owned()),
        )?;

        Ok(Self {
            bind_address,
            max_request_bytes,
            llm_gateway_url,
            llm_gateway_admin_token,
            llm_gateway_timeout: Duration::from_secs(timeout_seconds),
            execution_gateway_url,
            execution_gateway_control_token,
            execution_gateway_timeout: Duration::from_secs(execution_gateway_timeout_seconds),
            chatgpt_oauth_callback_address,
            chatgpt_oauth_client_id,
            chatgpt_oauth_issuer,
            chatgpt_oauth_redirect_uri,
            dashboard_origin,
        })
    }
}

fn parse_http_url(name: &'static str, value: &str) -> Result<Url, ConfigError> {
    let url = Url::parse(value).map_err(|source| ConfigError::InvalidUrl { name, source })?;
    if !matches!(url.scheme(), "http" | "https") {
        return Err(ConfigError::UnsupportedUrlScheme {
            name,
            scheme: url.scheme().to_owned(),
        });
    }
    Ok(url)
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

fn require_positive<T>(name: &'static str, value: T) -> Result<(), ConfigError>
where
    T: Default + PartialEq,
{
    if value == T::default() {
        return Err(ConfigError::NotPositive(name));
    }
    Ok(())
}

#[derive(Debug, thiserror::Error)]
pub enum ConfigError {
    #[error(
        "PLATFORM_LLM_GATEWAY_ADMIN_TOKEN or GATEWAY_ADMIN_TOKEN must contain the gateway admin token"
    )]
    MissingLlmGatewayAdminToken,
    #[error(
        "PLATFORM_EXECUTION_GATEWAY_CONTROL_TOKEN or EXECUTION_GATEWAY_CONTROL_TOKEN must contain the execution gateway control token"
    )]
    MissingExecutionGatewayControlToken,
    #[error("PLATFORM_BIND_ADDRESS is not a valid socket address")]
    InvalidBindAddress(#[source] std::net::AddrParseError),
    #[error("PLATFORM_CHATGPT_OAUTH_CALLBACK_ADDRESS is not a valid socket address")]
    InvalidChatGptCallbackAddress(#[source] std::net::AddrParseError),
    #[error("PLATFORM_LLM_GATEWAY_URL is not a valid URL")]
    InvalidLlmGatewayUrl(#[source] url::ParseError),
    #[error("PLATFORM_LLM_GATEWAY_URL must use http or https, not {0:?}")]
    UnsupportedLlmGatewayUrlScheme(String),
    #[error("{name} is not a valid URL")]
    InvalidUrl {
        name: &'static str,
        #[source]
        source: url::ParseError,
    },
    #[error("{name} must use http or https, not {scheme:?}")]
    UnsupportedUrlScheme { name: &'static str, scheme: String },
    #[error("{name} must be an integer, got {value:?}")]
    InvalidInteger {
        name: &'static str,
        value: String,
        #[source]
        source: ParseIntError,
    },
    #[error("{0} must be greater than zero")]
    NotPositive(&'static str),
}
