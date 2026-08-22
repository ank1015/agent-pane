use std::{fmt, time::Duration};

use llm_contracts::LlmError;

use crate::{DEFAULT_FIREWORKS_BASE_URL, error::invalid_config};

/// Default timeout for a complete non-streaming Fireworks response.
pub const DEFAULT_FIREWORKS_TIMEOUT: Duration = Duration::from_secs(15 * 60);

/// HTTP and authentication configuration for the Fireworks provider.
#[derive(Clone)]
pub struct FireworksConfig {
    pub(crate) api_key: String,
    pub(crate) base_url: String,
    pub(crate) timeout: Duration,
}

impl FireworksConfig {
    /// Creates configuration with the standard Fireworks inference URL.
    pub fn new(api_key: impl Into<String>) -> Result<Self, LlmError> {
        let api_key = api_key.into();
        if api_key.trim().is_empty() {
            return Err(invalid_config("Fireworks API key must not be empty."));
        }
        Ok(Self {
            api_key,
            base_url: DEFAULT_FIREWORKS_BASE_URL.to_owned(),
            timeout: DEFAULT_FIREWORKS_TIMEOUT,
        })
    }

    /// Changes the API base URL, primarily for tests and compatible proxies.
    pub fn with_base_url(mut self, base_url: impl Into<String>) -> Result<Self, LlmError> {
        let base_url = base_url.into();
        let parsed = url::Url::parse(&base_url)
            .map_err(|_| invalid_config("Fireworks base URL must be a fully qualified URL."))?;
        if !parsed.has_host() {
            return Err(invalid_config(
                "Fireworks base URL must be a fully qualified URL.",
            ));
        }
        self.base_url = base_url.trim_end_matches('/').to_owned();
        Ok(self)
    }

    #[must_use]
    pub fn with_timeout(mut self, timeout: Duration) -> Self {
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

impl fmt::Debug for FireworksConfig {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("FireworksConfig")
            .field("api_key", &"[REDACTED]")
            .field("base_url", &self.base_url)
            .field("timeout", &self.timeout)
            .finish()
    }
}
