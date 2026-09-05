use std::time::Duration;

use reqwest::header::HeaderValue;
use url::Url;
use zeroize::Zeroizing;

/// Shared connection settings. Credentials are excluded from Debug and serialization.
pub struct LlmClientConfig {
    /// Deployment root, optionally with a path prefix. Do not append `/v1`.
    pub base_url: Url,
    api_token: Zeroizing<String>,
    /// Complete HTTP request timeout, including response body reads.
    pub request_timeout: Duration,
    pub max_response_bytes: usize,
    pub allow_insecure_http: bool,
}

impl LlmClientConfig {
    #[must_use]
    pub fn new(base_url: Url, api_token: impl Into<String>) -> Self {
        Self {
            base_url,
            api_token: Zeroizing::new(api_token.into()),
            request_timeout: Duration::from_secs(35),
            max_response_bytes: 32 * 1024 * 1024,
            allow_insecure_http: false,
        }
    }

    pub(crate) fn authorization(&self) -> Result<HeaderValue, ConfigError> {
        if !matches!(self.base_url.scheme(), "http" | "https") || self.base_url.host_str().is_none()
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
                "base URL must not contain credentials, query, or fragment",
            ));
        }
        if self.request_timeout.is_zero() || self.max_response_bytes == 0 {
            return Err(ConfigError::Invalid(
                "timeout and response byte limit must be positive",
            ));
        }
        if self.api_token.is_empty() || !self.api_token.bytes().all(|b| b.is_ascii_graphic()) {
            return Err(ConfigError::Invalid(
                "API token must contain visible ASCII without spaces",
            ));
        }
        let bearer = Zeroizing::new(format!("Bearer {}", self.api_token.as_str()));
        let mut authorization = HeaderValue::from_str(&bearer)
            .map_err(|_| ConfigError::Invalid("invalid bearer credential"))?;
        authorization.set_sensitive(true);
        Ok(authorization)
    }
}

#[derive(Debug, thiserror::Error)]
pub enum ConfigError {
    #[error("invalid LLM client configuration: {0}")]
    Invalid(&'static str),
    #[error("failed to build LLM gateway HTTP client")]
    BuildClient(#[source] reqwest::Error),
}

/// Bounds the total wait independently of individual HTTP requests.
#[derive(Clone, Copy, Debug)]
pub struct WaitOptions {
    pub timeout: Duration,
}

impl WaitOptions {
    #[must_use]
    pub fn new(timeout: Duration) -> Self {
        Self { timeout }
    }
}

impl Default for WaitOptions {
    fn default() -> Self {
        Self::new(Duration::from_secs(10 * 60))
    }
}
