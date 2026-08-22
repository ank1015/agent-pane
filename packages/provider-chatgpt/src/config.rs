use std::{fmt, time::Duration};

use llm_contracts::LlmError;

use crate::{DEFAULT_CHATGPT_BASE_URL, error::invalid_config};

/// Default timeout for a complete buffered ChatGPT response.
pub const DEFAULT_CHATGPT_TIMEOUT: Duration = Duration::from_secs(15 * 60);

/// HTTP and caller-owned credential configuration for the ChatGPT backend.
#[derive(Clone)]
pub struct ChatGptConfig {
    pub(crate) access_token: String,
    pub(crate) account_id: String,
    pub(crate) base_url: String,
    pub(crate) timeout: Duration,
}

impl ChatGptConfig {
    /// Creates configuration from a ChatGPT OAuth access token and account ID.
    pub fn new(
        access_token: impl Into<String>,
        account_id: impl Into<String>,
    ) -> Result<Self, LlmError> {
        let access_token = access_token.into();
        let account_id = account_id.into();
        if access_token.trim().is_empty() {
            return Err(invalid_config(
                "ChatGPT OAuth access token must not be empty.",
            ));
        }
        if account_id.trim().is_empty() {
            return Err(invalid_config("ChatGPT account ID must not be empty."));
        }
        Ok(Self {
            access_token,
            account_id,
            base_url: DEFAULT_CHATGPT_BASE_URL.to_owned(),
            timeout: DEFAULT_CHATGPT_TIMEOUT,
        })
    }

    /// Changes the backend base URL, primarily for tests and compatible proxies.
    pub fn with_base_url(mut self, base_url: impl Into<String>) -> Result<Self, LlmError> {
        let base_url = base_url.into();
        let parsed = url::Url::parse(&base_url)
            .map_err(|_| invalid_config("ChatGPT base URL must be a fully qualified URL."))?;
        if !parsed.has_host() {
            return Err(invalid_config(
                "ChatGPT base URL must be a fully qualified URL.",
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

impl fmt::Debug for ChatGptConfig {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("ChatGptConfig")
            .field("access_token", &"[REDACTED]")
            .field("account_id", &"[REDACTED]")
            .field("base_url", &self.base_url)
            .field("timeout", &self.timeout)
            .finish()
    }
}
