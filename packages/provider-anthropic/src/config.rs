use std::{fmt, time::Duration};

use llm_contracts::LlmError;

use crate::{DEFAULT_ANTHROPIC_BASE_URL, error::invalid_config};

/// Stable Anthropic Messages API version sent with every request.
pub const ANTHROPIC_API_VERSION: &str = "2023-06-01";

/// Default timeout for one complete non-streaming Anthropic response.
pub const DEFAULT_ANTHROPIC_TIMEOUT: Duration = Duration::from_secs(30 * 60);

/// HTTP and authentication configuration for the Anthropic provider.
#[derive(Clone)]
pub struct AnthropicConfig {
    pub(crate) credential: AnthropicCredential,
    pub(crate) base_url: String,
    pub(crate) api_version: String,
    pub(crate) beta_header: Option<String>,
    pub(crate) timeout: Duration,
}

#[derive(Clone)]
pub(crate) enum AnthropicCredential {
    ApiKey(String),
    BearerToken(String),
}

impl AnthropicConfig {
    /// Creates configuration with the standard Anthropic URL and API version.
    pub fn new(api_key: impl Into<String>) -> Result<Self, LlmError> {
        let api_key = validate_credential(api_key, "API key")?;
        Ok(Self {
            credential: AnthropicCredential::ApiKey(api_key),
            base_url: DEFAULT_ANTHROPIC_BASE_URL.to_owned(),
            api_version: ANTHROPIC_API_VERSION.to_owned(),
            beta_header: None,
            timeout: DEFAULT_ANTHROPIC_TIMEOUT,
        })
    }

    /// Creates configuration for an Anthropic-compatible endpoint that uses
    /// `Authorization: Bearer`, such as Claude Platform on AWS.
    pub fn from_bearer_token(token: impl Into<String>) -> Result<Self, LlmError> {
        let token = validate_credential(token, "bearer token")?;
        Ok(Self {
            credential: AnthropicCredential::BearerToken(token),
            base_url: DEFAULT_ANTHROPIC_BASE_URL.to_owned(),
            api_version: ANTHROPIC_API_VERSION.to_owned(),
            beta_header: None,
            timeout: DEFAULT_ANTHROPIC_TIMEOUT,
        })
    }

    /// Changes the API base URL, primarily for tests and compatible proxies.
    pub fn with_base_url(mut self, base_url: impl Into<String>) -> Result<Self, LlmError> {
        let base_url = base_url.into();
        let parsed = url::Url::parse(&base_url)
            .map_err(|_| invalid_config("Anthropic base URL must be a fully qualified URL."))?;
        if !parsed.has_host() {
            return Err(invalid_config(
                "Anthropic base URL must be a fully qualified URL.",
            ));
        }
        self.base_url = base_url.trim_end_matches('/').to_owned();
        Ok(self)
    }

    /// Overrides the required `anthropic-version` header.
    pub fn with_api_version(mut self, api_version: impl Into<String>) -> Result<Self, LlmError> {
        let api_version = api_version.into();
        if api_version.trim().is_empty() {
            return Err(invalid_config("Anthropic API version must not be empty."));
        }
        self.api_version = api_version;
        Ok(self)
    }

    /// Sets the optional `anthropic-beta` header for hosted tools and beta features.
    pub fn with_beta_header(mut self, beta: impl Into<String>) -> Result<Self, LlmError> {
        let beta = beta.into();
        if beta.trim().is_empty() {
            return Err(invalid_config("Anthropic beta header must not be empty."));
        }
        self.beta_header = Some(beta);
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
    pub fn api_version(&self) -> &str {
        &self.api_version
    }

    #[must_use]
    pub fn beta_header(&self) -> Option<&str> {
        self.beta_header.as_deref()
    }

    #[must_use]
    pub const fn timeout(&self) -> Duration {
        self.timeout
    }
}

impl fmt::Debug for AnthropicConfig {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("AnthropicConfig")
            .field("credential", &"[REDACTED]")
            .field("base_url", &self.base_url)
            .field("api_version", &self.api_version)
            .field("beta_header", &self.beta_header)
            .field("timeout", &self.timeout)
            .finish()
    }
}

fn validate_credential(value: impl Into<String>, name: &str) -> Result<String, LlmError> {
    let value = value.into();
    if value.trim().is_empty() {
        return Err(invalid_config(format!(
            "Anthropic {name} must not be empty."
        )));
    }
    Ok(value)
}
