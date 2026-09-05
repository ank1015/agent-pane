use std::time::{Instant, SystemTime};

use async_trait::async_trait;
use llm_contracts::{
    AssistantMessage, LlmError, LlmProviderAdapter, LlmRequest, LlmTransport, ProviderId,
    SearchRequest, SearchRequestOptions, SearchResponse, SearchTransport, Validate,
};
use reqwest::header::{ACCEPT, AUTHORIZATION, CONTENT_TYPE, HeaderValue, RETRY_AFTER, USER_AGENT};

use crate::{
    OPENAI_PROVIDER,
    config::OpenAiConfig,
    error::{
        invalid_config, invalid_model, invalid_request, invalid_response, network_error,
        normalize_http_error, unix_millis,
    },
    find_model,
    request::build_response_request_for_model,
    response::convert_response,
};

const ORIGINATOR: &str = "agent-pane";
const MAX_RESPONSE_BYTES: usize = 16 * 1024 * 1024;

/// Non-streaming OpenAI Responses API client.
#[derive(Clone, Debug)]
pub struct OpenAiProvider {
    client: reqwest::Client,
    config: OpenAiConfig,
    provider_id: ProviderId,
}

impl OpenAiProvider {
    /// Creates a provider with a default HTTP client.
    pub fn new(config: OpenAiConfig) -> Result<Self, LlmError> {
        let client = reqwest::Client::builder()
            .redirect(reqwest::redirect::Policy::none())
            .timeout(config.timeout)
            .build()
            .map_err(|error| invalid_config(error.to_string()))?;
        Ok(Self::with_client(config, client))
    }

    /// Convenience constructor using standard configuration.
    pub fn from_api_key(api_key: impl Into<String>) -> Result<Self, LlmError> {
        Self::new(OpenAiConfig::new(api_key)?)
    }

    /// Creates a provider with an application-owned HTTP client.
    #[must_use]
    pub fn with_client(config: OpenAiConfig, client: reqwest::Client) -> Self {
        Self {
            client,
            config,
            provider_id: ProviderId::new(OPENAI_PROVIDER)
                .expect("the built-in OpenAI provider id is valid"),
        }
    }

    /// Returns the configured API base URL.
    #[must_use]
    pub fn base_url(&self) -> &str {
        self.config.base_url()
    }
}

#[async_trait]
impl LlmTransport for OpenAiProvider {
    async fn complete(&self, request: LlmRequest) -> Result<AssistantMessage, LlmError> {
        request
            .validate()
            .map_err(|error| invalid_request(error.to_string()))?;
        if request.model.provider.as_str() != OPENAI_PROVIDER {
            return Err(invalid_request(format!(
                "OpenAI provider cannot handle model provider `{}`.",
                request.model.provider
            )));
        }
        let model = find_model(request.model.id.as_str())
            .ok_or_else(|| invalid_model(request.model.id.as_str()))?;
        let body = build_response_request_for_model(&request, model)?;
        let started = Instant::now();

        let mut http_request = self
            .client
            .post(format!("{}/responses", self.config.base_url))
            .header(ACCEPT, "application/json")
            .header(CONTENT_TYPE, "application/json")
            .header(AUTHORIZATION, bearer_header(&self.config.api_key)?)
            .json(&body);
        if crate::uses_codex_responses_lite(&request) {
            http_request = http_request.header("x-openai-internal-codex-responses-lite", "true");
        }
        if let Some(organization) = &self.config.organization {
            http_request = http_request.header("openai-organization", organization);
        }
        if let Some(project) = &self.config.project {
            http_request = http_request.header("openai-project", project);
        }

        let mut response = http_request
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
        let response_body = read_response_body(&mut response).await?;
        if !status.is_success() {
            return Err(normalize_http_error(
                status.as_u16(),
                &status_text,
                &response_body,
                retry_after.as_deref(),
                SystemTime::now(),
            ));
        }

        let native =
            serde_json::from_str(&response_body).map_err(|error| llm_contracts::LlmError {
                message: format!("OpenAI returned invalid JSON: {error}"),
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

#[async_trait]
impl SearchTransport for OpenAiProvider {
    async fn search(
        &self,
        request: SearchRequest,
        options: SearchRequestOptions,
    ) -> Result<SearchResponse, LlmError> {
        request
            .validate()
            .map_err(|error| invalid_request(error.to_string()))?;
        if find_model(&request.model).is_none() {
            return Err(invalid_model(&request.model));
        }

        let mut http_request = self
            .client
            .post(format!("{}/alpha/search", self.config.base_url))
            .header(ACCEPT, "application/json")
            .header(CONTENT_TYPE, "application/json")
            .header(AUTHORIZATION, bearer_header(&self.config.api_key)?)
            .header(
                USER_AGENT,
                concat!("agent-pane-provider-openai/", env!("CARGO_PKG_VERSION")),
            )
            .json(&request);
        let originator = options.originator.as_deref().unwrap_or(ORIGINATOR);
        http_request = http_request.header("originator", request_header(originator)?);
        if let Some(metadata) = options.codex_turn_metadata.as_deref() {
            http_request = http_request.header("x-codex-turn-metadata", request_header(metadata)?);
        }
        if let Some(organization) = &self.config.organization {
            http_request = http_request.header("openai-organization", organization);
        }
        if let Some(project) = &self.config.project {
            http_request = http_request.header("openai-project", project);
        }

        let mut response = http_request
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
        let response_body = read_response_body(&mut response).await?;
        if !status.is_success() {
            return Err(normalize_http_error(
                status.as_u16(),
                &status_text,
                &response_body,
                retry_after.as_deref(),
                SystemTime::now(),
            ));
        }

        serde_json::from_str(&response_body).map_err(|error| {
            invalid_response(
                format!("OpenAI returned an invalid alpha/search response: {error}"),
                serde_json::Value::String(response_body),
            )
        })
    }
}

impl LlmProviderAdapter for OpenAiProvider {
    fn provider(&self) -> &ProviderId {
        &self.provider_id
    }
}

fn bearer_header(api_key: &str) -> Result<HeaderValue, LlmError> {
    let mut value = HeaderValue::from_str(&format!("Bearer {api_key}"))
        .map_err(|_| invalid_config("OpenAI API key contains invalid header characters."))?;
    value.set_sensitive(true);
    Ok(value)
}

fn request_header(value: &str) -> Result<HeaderValue, LlmError> {
    HeaderValue::from_str(value)
        .map_err(|_| invalid_request("search request metadata contains invalid header characters"))
}

async fn read_response_body(response: &mut reqwest::Response) -> Result<String, LlmError> {
    let mut body = Vec::new();
    while let Some(chunk) = response
        .chunk()
        .await
        .map_err(|error| network_error(&error))?
    {
        if body.len().saturating_add(chunk.len()) > MAX_RESPONSE_BYTES {
            return Err(response_too_large());
        }
        body.extend_from_slice(&chunk);
    }
    String::from_utf8(body).map_err(|_| {
        invalid_response(
            "OpenAI returned a response that was not valid UTF-8",
            serde_json::Value::Null,
        )
    })
}

fn response_too_large() -> LlmError {
    invalid_response(
        format!("OpenAI response exceeded the {MAX_RESPONSE_BYTES}-byte safety limit"),
        serde_json::Value::Null,
    )
}
