use std::time::{Instant, SystemTime};

use async_trait::async_trait;
use llm_contracts::{
    AssistantMessage, LlmError, LlmProviderAdapter, LlmRequest, LlmTransport, ProviderId, Validate,
};
use reqwest::header::{ACCEPT, AUTHORIZATION, CONTENT_TYPE, HeaderValue, RETRY_AFTER};

use crate::{
    ANTHROPIC_PROVIDER,
    config::{AnthropicConfig, AnthropicCredential},
    error::{
        invalid_config, invalid_model, invalid_request, network_error, normalize_http_error,
        unix_millis,
    },
    find_model,
    request::build_message_request_for_model,
    response::convert_response,
};

/// Non-streaming Anthropic Messages client.
#[derive(Clone, Debug)]
pub struct AnthropicProvider {
    client: reqwest::Client,
    config: AnthropicConfig,
    provider_id: ProviderId,
}

impl AnthropicProvider {
    /// Creates a provider with a default HTTP client and 30-minute timeout.
    pub fn new(config: AnthropicConfig) -> Result<Self, LlmError> {
        let client = reqwest::Client::builder()
            .timeout(config.timeout)
            .build()
            .map_err(|error| invalid_config(error.to_string()))?;
        Ok(Self::with_client(config, client))
    }

    /// Convenience constructor using standard configuration.
    pub fn from_api_key(api_key: impl Into<String>) -> Result<Self, LlmError> {
        Self::new(AnthropicConfig::new(api_key)?)
    }

    /// Convenience constructor for compatible endpoints using bearer auth.
    pub fn from_bearer_token(token: impl Into<String>) -> Result<Self, LlmError> {
        Self::new(AnthropicConfig::from_bearer_token(token)?)
    }

    /// Creates a provider with an application-owned HTTP client.
    #[must_use]
    pub fn with_client(config: AnthropicConfig, client: reqwest::Client) -> Self {
        Self {
            client,
            config,
            provider_id: ProviderId::new(ANTHROPIC_PROVIDER)
                .expect("the built-in Anthropic provider id is valid"),
        }
    }

    /// Returns the configured API base URL.
    #[must_use]
    pub fn base_url(&self) -> &str {
        self.config.base_url()
    }
}

#[async_trait]
impl LlmTransport for AnthropicProvider {
    async fn complete(&self, request: LlmRequest) -> Result<AssistantMessage, LlmError> {
        request
            .validate()
            .map_err(|error| invalid_request(error.to_string()))?;
        if request.model.provider.as_str() != ANTHROPIC_PROVIDER {
            return Err(invalid_request(format!(
                "Anthropic provider cannot handle model provider `{}`.",
                request.model.provider
            )));
        }
        let model = find_model(request.model.id.as_str())
            .ok_or_else(|| invalid_model(request.model.id.as_str()))?;
        let body = build_message_request_for_model(&request, model)?;
        let started = Instant::now();

        let mut builder = self
            .client
            .post(format!("{}/messages", self.config.base_url))
            .header(ACCEPT, "application/json")
            .header(CONTENT_TYPE, "application/json")
            .header(
                "anthropic-version",
                header(&self.config.api_version, "API version")?,
            );
        builder = match &self.config.credential {
            AnthropicCredential::ApiKey(api_key) => {
                builder.header("x-api-key", sensitive_header(api_key, "API key")?)
            }
            AnthropicCredential::BearerToken(token) => builder.header(
                AUTHORIZATION,
                sensitive_header(&format!("Bearer {token}"), "bearer token")?,
            ),
        };
        if let Some(beta) = &self.config.beta_header {
            builder = builder.header("anthropic-beta", header(beta, "beta header")?);
        }
        let response = builder
            .json(&body)
            .send()
            .await
            .map_err(|error| network_error(&error))?;
        let status = response.status();
        let status_text = status.canonical_reason().unwrap_or_default().to_owned();
        let retry_after = response
            .headers()
            .get(RETRY_AFTER)
            .and_then(|value| value.to_str().ok())
            .map(ToOwned::to_owned);
        let response_body = response
            .text()
            .await
            .map_err(|error| network_error(&error))?;
        if !status.is_success() {
            return Err(normalize_http_error(
                status.as_u16(),
                &status_text,
                &response_body,
                retry_after.as_deref(),
                SystemTime::now(),
            ));
        }

        let native = serde_json::from_str(&response_body).map_err(|error| LlmError {
            message: format!("Anthropic returned invalid JSON: {error}"),
            provider_code: None,
            provider_type: Some("invalid_response".to_owned()),
            http_status: Some(status.as_u16()),
            can_retry: false,
            retry_after_ms: None,
            native_error: Some(Box::new(serde_json::Value::String(response_body))),
        })?;
        let duration_ms = started.elapsed().as_millis().try_into().unwrap_or(u64::MAX);
        convert_response(native, model, duration_ms, unix_millis(SystemTime::now()))
    }
}

impl LlmProviderAdapter for AnthropicProvider {
    fn provider(&self) -> &ProviderId {
        &self.provider_id
    }
}

fn header(value: &str, field: &str) -> Result<HeaderValue, LlmError> {
    HeaderValue::from_str(value).map_err(|_| {
        invalid_config(format!(
            "Anthropic {field} contains invalid header characters."
        ))
    })
}

fn sensitive_header(value: &str, field: &str) -> Result<HeaderValue, LlmError> {
    let mut value = header(value, field)?;
    value.set_sensitive(true);
    Ok(value)
}
