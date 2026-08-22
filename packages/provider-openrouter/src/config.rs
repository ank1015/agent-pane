use std::{fmt, time::Duration};

use llm_contracts::LlmError;

use crate::{DEFAULT_OPENROUTER_BASE_URL, error::invalid_config};

/// Default timeout for one complete non-streaming response.
pub const DEFAULT_OPENROUTER_TIMEOUT: Duration = Duration::from_secs(30 * 60);

/// HTTP, authentication, and optional application attribution configuration.
#[derive(Clone)]
pub struct OpenRouterConfig {
    pub(crate) api_key: String,
    pub(crate) base_url: String,
    pub(crate) http_referer: Option<String>,
    pub(crate) app_title: Option<String>,
    pub(crate) router_metadata: bool,
    pub(crate) timeout: Duration,
}

impl OpenRouterConfig {
    /// Creates configuration for the standard OpenRouter API.
    pub fn new(api_key: impl Into<String>) -> Result<Self, LlmError> {
        let api_key = api_key.into();
        if api_key.trim().is_empty() {
            return Err(invalid_config("OpenRouter API key must not be empty."));
        }
        Ok(Self {
            api_key,
            base_url: DEFAULT_OPENROUTER_BASE_URL.to_owned(),
            http_referer: None,
            app_title: None,
            router_metadata: false,
            timeout: DEFAULT_OPENROUTER_TIMEOUT,
        })
    }

    /// Changes the API base URL, primarily for tests and compatible proxies.
    pub fn with_base_url(mut self, base_url: impl Into<String>) -> Result<Self, LlmError> {
        let base_url = base_url.into();
        let parsed = url::Url::parse(&base_url)
            .map_err(|_| invalid_config("OpenRouter base URL must be a fully qualified URL."))?;
        if !parsed.has_host() {
            return Err(invalid_config(
                "OpenRouter base URL must be a fully qualified URL.",
            ));
        }
        self.base_url = base_url.trim_end_matches('/').to_owned();
        Ok(self)
    }

    /// Adds OpenRouter's optional `HTTP-Referer` application attribution header.
    pub fn with_http_referer(mut self, referer: impl Into<String>) -> Result<Self, LlmError> {
        let referer = referer.into();
        let parsed = url::Url::parse(&referer)
            .map_err(|_| invalid_config("OpenRouter HTTP referer must be a valid URL."))?;
        if !parsed.has_host() {
            return Err(invalid_config(
                "OpenRouter HTTP referer must be a valid URL.",
            ));
        }
        self.http_referer = Some(referer);
        Ok(self)
    }

    /// Adds OpenRouter's optional `X-OpenRouter-Title` attribution header.
    pub fn with_app_title(mut self, title: impl Into<String>) -> Result<Self, LlmError> {
        let title = title.into();
        if title.trim().is_empty() {
            return Err(invalid_config(
                "OpenRouter application title must not be empty.",
            ));
        }
        self.app_title = Some(title);
        Ok(self)
    }

    /// Requests OpenRouter's router metadata in supported responses.
    #[must_use]
    pub const fn with_router_metadata(mut self, enabled: bool) -> Self {
        self.router_metadata = enabled;
        self
    }

    #[must_use]
    pub const fn with_timeout(mut self, timeout: Duration) -> Self {
        self.timeout = timeout;
        self
    }

    #[must_use]
    pub fn base_url(&self) -> &str {
        &self.base_url
    }

    #[must_use]
    pub const fn timeout(&self) -> Duration {
        self.timeout
    }
}

impl fmt::Debug for OpenRouterConfig {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("OpenRouterConfig")
            .field("api_key", &"[REDACTED]")
            .field("base_url", &self.base_url)
            .field("http_referer", &self.http_referer)
            .field("app_title", &self.app_title)
            .field("router_metadata", &self.router_metadata)
            .field("timeout", &self.timeout)
            .finish()
    }
}
