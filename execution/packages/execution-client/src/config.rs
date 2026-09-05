use std::time::Duration;

use reqwest::header::HeaderValue;
use thiserror::Error;
use url::Url;
use zeroize::Zeroizing;

/// Connection settings shared by every host selected through a client.
///
/// The base URL is the gateway deployment root, optionally with a path prefix;
/// the client appends `v1/hosts/{host_id}/operations`. Do not include `/v1`.
/// This type deliberately does not implement `Debug` or serialization.
pub struct ExecutionClientConfig {
    pub base_url: Url,
    api_token: Zeroizing<String>,
    /// Upper bound for a complete request, including reading its response body.
    /// A shorter operation-context deadline always takes precedence.
    pub request_timeout: Duration,
    /// Maximum encoded response size, including JSON and Base64 overhead.
    pub max_response_bytes: usize,
    /// Explicit opt-in for development or tests using an HTTP gateway.
    pub allow_insecure_http: bool,
}

impl ExecutionClientConfig {
    #[must_use]
    pub fn new(base_url: Url, api_token: impl Into<String>) -> Self {
        Self {
            base_url,
            api_token: Zeroizing::new(api_token.into()),
            request_timeout: Duration::from_secs(60),
            max_response_bytes: 32 * 1024 * 1024,
            allow_insecure_http: false,
        }
    }

    pub(crate) fn authorization(&self) -> Result<HeaderValue, ConfigError> {
        if !matches!(self.base_url.scheme(), "http" | "https")
            || self.base_url.host_str().is_none()
            || self.base_url.cannot_be_a_base()
        {
            return Err(ConfigError::Invalid(
                "base URL must be an HTTP(S) gateway URL",
            ));
        }
        if self.base_url.scheme() == "http" && !self.allow_insecure_http {
            return Err(ConfigError::Invalid("HTTP requires allow_insecure_http"));
        }
        if !self.base_url.username().is_empty()
            || self.base_url.password().is_some()
            || self.base_url.query().is_some()
            || self.base_url.fragment().is_some()
        {
            return Err(ConfigError::Invalid(
                "base URL must not contain credentials, a query, or a fragment",
            ));
        }
        if self.request_timeout.is_zero() || self.max_response_bytes == 0 {
            return Err(ConfigError::Invalid(
                "timeout and response limit must be positive",
            ));
        }
        if self.api_token.is_empty() || !self.api_token.bytes().all(|byte| byte.is_ascii_graphic())
        {
            return Err(ConfigError::Invalid(
                "API token must be nonempty printable ASCII without spaces",
            ));
        }
        let bearer = Zeroizing::new(format!("Bearer {}", self.api_token.as_str()));
        let mut value = HeaderValue::from_str(&bearer)
            .map_err(|_| ConfigError::Invalid("API token is not a valid bearer credential"))?;
        value.set_sensitive(true);
        Ok(value)
    }
}

/// Failures constructing a client. Operation failures use execution-core errors.
#[derive(Debug, Error)]
pub enum ConfigError {
    #[error("invalid execution client configuration: {0}")]
    Invalid(&'static str),
    #[error("failed to build execution HTTP client")]
    BuildClient(#[source] reqwest::Error),
}
