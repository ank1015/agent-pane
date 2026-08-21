use std::{fmt, time::Duration};

use llm_contracts::LlmError;

use crate::{DEFAULT_OPENAI_BASE_URL, error::invalid_config};

/// Default timeout for synchronous, non-streaming model responses.
pub const DEFAULT_OPENAI_TIMEOUT: Duration = Duration::from_secs(15 * 60);

/// HTTP and authentication configuration for the OpenAI provider.
#[derive(Clone)]
pub struct OpenAiConfig {
    pub(crate) api_key: String,
    pub(crate) base_url: String,
    pub(crate) organization: Option<String>,
    pub(crate) project: Option<String>,
    pub(crate) timeout: Duration,
}

impl OpenAiConfig {
    /// Creates configuration with the standard OpenAI API URL.
    pub fn new(api_key: impl Into<String>) -> Result<Self, LlmError> {
        let api_key = api_key.into();
        if api_key.trim().is_empty() {
            return Err(invalid_config("OpenAI API key must not be empty."));
        }
        Ok(Self {
            api_key,
            base_url: DEFAULT_OPENAI_BASE_URL.to_owned(),
            organization: None,
            project: None,
            timeout: DEFAULT_OPENAI_TIMEOUT,
        })
    }

    /// Changes the API base URL, primarily for compatible gateways and tests.
    pub fn with_base_url(mut self, base_url: impl Into<String>) -> Result<Self, LlmError> {
        let base_url = base_url.into();
        let parsed = url::Url::parse(&base_url)
            .map_err(|_| invalid_config("OpenAI base URL must be a fully qualified URL."))?;
        if !parsed.has_host() {
            return Err(invalid_config(
                "OpenAI base URL must be a fully qualified URL.",
            ));
        }
        self.base_url = base_url.trim_end_matches('/').to_owned();
        Ok(self)
    }

    #[must_use]
    pub fn with_organization(mut self, organization: impl Into<String>) -> Self {
        self.organization = non_empty_option(organization.into());
        self
    }

    #[must_use]
    pub fn with_project(mut self, project: impl Into<String>) -> Self {
        self.project = non_empty_option(project.into());
        self
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
    pub fn organization(&self) -> Option<&str> {
        self.organization.as_deref()
    }

    #[must_use]
    pub fn project(&self) -> Option<&str> {
        self.project.as_deref()
    }

    #[must_use]
    pub const fn timeout(&self) -> Duration {
        self.timeout
    }
}

impl fmt::Debug for OpenAiConfig {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("OpenAiConfig")
            .field("api_key", &"[REDACTED]")
            .field("base_url", &self.base_url)
            .field("organization", &self.organization)
            .field("project", &self.project)
            .field("timeout", &self.timeout)
            .finish()
    }
}

fn non_empty_option(value: String) -> Option<String> {
    (!value.trim().is_empty()).then_some(value)
}
