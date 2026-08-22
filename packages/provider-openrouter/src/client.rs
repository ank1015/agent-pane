use std::time::{Instant, SystemTime};

use async_trait::async_trait;
use llm_contracts::{
    AssistantMessage, LlmError, LlmProviderAdapter, LlmRequest, LlmTransport, ProviderId, Validate,
};
use reqwest::header::{ACCEPT, AUTHORIZATION, CONTENT_TYPE, HeaderName, HeaderValue, RETRY_AFTER};

use crate::{
    OPENROUTER_PROVIDER,
    config::OpenRouterConfig,
    error::{
        invalid_config, invalid_model, invalid_request, network_error, normalize_http_error,
        unix_millis,
    },
    find_model,
    request::build_chat_completion_request_for_model,
    response::convert_response,
};

const HTTP_REFERER: HeaderName = HeaderName::from_static("http-referer");
const OPENROUTER_TITLE: HeaderName = HeaderName::from_static("x-openrouter-title");
const OPENROUTER_METADATA: HeaderName = HeaderName::from_static("x-openrouter-metadata");

/// Non-streaming OpenRouter Chat Completions client.
#[derive(Clone, Debug)]
pub struct OpenRouterProvider {
    client: reqwest::Client,
    config: OpenRouterConfig,
    provider_id: ProviderId,
}

impl OpenRouterProvider {
    /// Creates a provider with a default HTTP client.
    pub fn new(config: OpenRouterConfig) -> Result<Self, LlmError> {
        let client = reqwest::Client::builder()
            .timeout(config.timeout)
            .build()
            .map_err(|error| invalid_config(error.to_string()))?;
        Ok(Self::with_client(config, client))
    }

    /// Convenience constructor using standard configuration.
    pub fn from_api_key(api_key: impl Into<String>) -> Result<Self, LlmError> {
        Self::new(OpenRouterConfig::new(api_key)?)
    }

    /// Creates a provider with an application-owned HTTP client.
    #[must_use]
    pub fn with_client(config: OpenRouterConfig, client: reqwest::Client) -> Self {
        Self {
            client,
            config,
            provider_id: ProviderId::new(OPENROUTER_PROVIDER)
                .expect("the built-in OpenRouter provider id is valid"),
        }
    }

    /// Returns the configured API base URL.
    #[must_use]
    pub fn base_url(&self) -> &str {
        self.config.base_url()
    }
}

#[async_trait]
impl LlmTransport for OpenRouterProvider {
    async fn complete(&self, request: LlmRequest) -> Result<AssistantMessage, LlmError> {
        request
            .validate()
            .map_err(|error| invalid_request(error.to_string()))?;
        if request.model.provider.as_str() != OPENROUTER_PROVIDER {
            return Err(invalid_request(format!(
                "OpenRouter provider cannot handle model provider `{}`.",
                request.model.provider
            )));
        }
        let model = find_model(request.model.id.as_str())
            .ok_or_else(|| invalid_model(request.model.id.as_str()))?;
        let body = build_chat_completion_request_for_model(&request, model)?;
        let started = Instant::now();

        let mut http_request = self
            .client
            .post(format!("{}/chat/completions", self.config.base_url))
            .header(ACCEPT, "application/json")
            .header(CONTENT_TYPE, "application/json")
            .header(AUTHORIZATION, bearer_header(&self.config.api_key)?)
            .json(&body);
        if let Some(referer) = &self.config.http_referer {
            http_request =
                http_request.header(HTTP_REFERER, header_value(referer, "HTTP referer")?);
        }
        if let Some(title) = &self.config.app_title {
            http_request =
                http_request.header(OPENROUTER_TITLE, header_value(title, "application title")?);
        }
        if self.config.router_metadata {
            http_request = http_request.header(OPENROUTER_METADATA, "enabled");
        }

        let response = http_request
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
            message: format!("OpenRouter returned invalid JSON: {error}"),
            provider_code: None,
            provider_type: Some("invalid_response".to_owned()),
            http_status: Some(status.as_u16()),
            can_retry: false,
            retry_after_ms: None,
            native_error: Some(Box::new(serde_json::Value::String(response_body))),
        })?;
        let now = SystemTime::now();
        let duration_ms = started.elapsed().as_millis().try_into().unwrap_or(u64::MAX);
        convert_response(native, model, duration_ms, unix_millis(now))
    }
}

impl LlmProviderAdapter for OpenRouterProvider {
    fn provider(&self) -> &ProviderId {
        &self.provider_id
    }
}

fn bearer_header(api_key: &str) -> Result<HeaderValue, LlmError> {
    let mut value = HeaderValue::from_str(&format!("Bearer {api_key}"))
        .map_err(|_| invalid_config("OpenRouter API key contains invalid header characters."))?;
    value.set_sensitive(true);
    Ok(value)
}

fn header_value(value: &str, description: &str) -> Result<HeaderValue, LlmError> {
    HeaderValue::from_str(value).map_err(|_| {
        invalid_config(format!(
            "OpenRouter {description} contains invalid header characters."
        ))
    })
}
