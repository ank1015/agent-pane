use std::time::{Instant, SystemTime};

use async_trait::async_trait;
use llm_contracts::{
    AssistantMessage, LlmError, LlmProviderAdapter, LlmRequest, LlmTransport, ProviderId,
    SearchRequest, SearchRequestOptions, SearchResponse, Validate,
};
use reqwest::header::{ACCEPT, AUTHORIZATION, CONTENT_TYPE, HeaderValue, RETRY_AFTER, USER_AGENT};

use crate::{
    CHATGPT_PROVIDER,
    config::ChatGptConfig,
    error::{
        invalid_config, invalid_model, invalid_request, invalid_response, network_error,
        normalize_http_error, unix_millis,
    },
    find_model,
    request::build_response_request,
    response::{convert_response_events, drain_sse_events, is_terminal_event},
};

const ORIGINATOR: &str = "agent-pane";
const OPENAI_BETA: &str = "responses=experimental";

/// ChatGPT Codex backend client with a buffered, non-streaming public API.
#[derive(Clone, Debug)]
pub struct ChatGptProvider {
    client: reqwest::Client,
    config: ChatGptConfig,
    provider_id: ProviderId,
}

impl ChatGptProvider {
    /// Creates a provider with a default HTTP client.
    pub fn new(config: ChatGptConfig) -> Result<Self, LlmError> {
        let client = reqwest::Client::builder()
            .timeout(config.timeout)
            .build()
            .map_err(|error| invalid_config(error.to_string()))?;
        Ok(Self::with_client(config, client))
    }

    /// Convenience constructor for caller-owned ChatGPT credentials.
    pub fn from_credentials(
        access_token: impl Into<String>,
        account_id: impl Into<String>,
    ) -> Result<Self, LlmError> {
        Self::new(ChatGptConfig::new(access_token, account_id)?)
    }

    /// Creates a provider with an application-owned HTTP client.
    #[must_use]
    pub fn with_client(config: ChatGptConfig, client: reqwest::Client) -> Self {
        Self {
            client,
            config,
            provider_id: ProviderId::new(CHATGPT_PROVIDER)
                .expect("the built-in ChatGPT provider id is valid"),
        }
    }

    /// Returns the configured backend base URL.
    #[must_use]
    pub fn base_url(&self) -> &str {
        self.config.base_url()
    }
}

#[async_trait]
impl LlmTransport for ChatGptProvider {
    async fn complete(&self, request: LlmRequest) -> Result<AssistantMessage, LlmError> {
        request
            .validate()
            .map_err(|error| invalid_request(error.to_string()))?;
        if request.model.provider.as_str() != CHATGPT_PROVIDER {
            return Err(invalid_request(format!(
                "ChatGPT provider cannot handle model provider `{}`.",
                request.model.provider
            )));
        }
        let model = find_model(request.model.id.as_str())
            .ok_or_else(|| invalid_model(request.model.id.as_str()))?;
        let body = build_response_request(&request)?;
        let started = Instant::now();

        let mut http_request = self
            .client
            .post(responses_url(self.config.base_url()))
            .header(ACCEPT, "text/event-stream")
            .header(CONTENT_TYPE, "application/json")
            .header(AUTHORIZATION, bearer_header(&self.config.access_token)?)
            .header(
                "chatgpt-account-id",
                sensitive_header(&self.config.account_id, "ChatGPT account ID")?,
            )
            .header("originator", ORIGINATOR)
            .header(
                USER_AGENT,
                concat!("agent-pane-provider-chatgpt/", env!("CARGO_PKG_VERSION")),
            )
            .header("openai-beta", OPENAI_BETA)
            .json(&body);
        if provider_openai::uses_codex_responses_lite(&request) {
            http_request = http_request.header("x-openai-internal-codex-responses-lite", "true");
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
        if !status.is_success() {
            let response_body = response
                .text()
                .await
                .map_err(|error| network_error(&error))?;
            return Err(normalize_http_error(
                status.as_u16(),
                &status_text,
                &response_body,
                retry_after.as_deref(),
                SystemTime::now(),
            ));
        }

        let mut buffer = Vec::new();
        let mut events = Vec::new();
        while let Some(chunk) = response
            .chunk()
            .await
            .map_err(|error| network_error(&error))?
        {
            buffer.extend_from_slice(&chunk);
            for event in drain_sse_events(&mut buffer, false)? {
                let terminal = is_terminal_event(&event);
                events.push(event);
                if terminal {
                    return finish(events, model, started);
                }
            }
        }
        for event in drain_sse_events(&mut buffer, true)? {
            let terminal = is_terminal_event(&event);
            events.push(event);
            if terminal {
                return finish(events, model, started);
            }
        }
        finish(events, model, started)
    }

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
            .post(search_url(self.config.base_url()))
            .header(ACCEPT, "application/json")
            .header(CONTENT_TYPE, "application/json")
            .header(AUTHORIZATION, bearer_header(&self.config.access_token)?)
            .header(
                "chatgpt-account-id",
                sensitive_header(&self.config.account_id, "ChatGPT account ID")?,
            )
            .header(
                USER_AGENT,
                concat!("agent-pane-provider-chatgpt/", env!("CARGO_PKG_VERSION")),
            )
            .json(&request);
        let originator = options.originator.as_deref().unwrap_or(ORIGINATOR);
        http_request = http_request.header("originator", request_header(originator)?);
        if let Some(metadata) = options.codex_turn_metadata.as_deref() {
            http_request = http_request.header("x-codex-turn-metadata", request_header(metadata)?);
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

        serde_json::from_str(&response_body).map_err(|error| {
            invalid_response(
                format!("ChatGPT returned an invalid alpha/search response: {error}"),
                serde_json::Value::String(response_body),
            )
        })
    }
}

impl LlmProviderAdapter for ChatGptProvider {
    fn provider(&self) -> &ProviderId {
        &self.provider_id
    }
}

fn finish(
    events: Vec<serde_json::Value>,
    model: &crate::ChatGptModel,
    started: Instant,
) -> Result<AssistantMessage, LlmError> {
    let now = SystemTime::now();
    let duration_ms = started.elapsed().as_millis().try_into().unwrap_or(u64::MAX);
    convert_response_events(events, model, duration_ms, unix_millis(now))
}

fn responses_url(base_url: &str) -> String {
    let normalized = base_url.trim_end_matches('/');
    if normalized.ends_with("/codex/responses") {
        normalized.to_owned()
    } else if normalized.ends_with("/codex") {
        format!("{normalized}/responses")
    } else {
        format!("{normalized}/codex/responses")
    }
}

fn search_url(base_url: &str) -> String {
    let normalized = base_url.trim_end_matches('/');
    if normalized.ends_with("/codex/alpha/search") {
        normalized.to_owned()
    } else if let Some(prefix) = normalized.strip_suffix("/codex/responses") {
        format!("{prefix}/codex/alpha/search")
    } else if normalized.ends_with("/codex") {
        format!("{normalized}/alpha/search")
    } else {
        format!("{normalized}/codex/alpha/search")
    }
}

fn bearer_header(access_token: &str) -> Result<HeaderValue, LlmError> {
    sensitive_header(
        &format!("Bearer {access_token}"),
        "ChatGPT OAuth access token",
    )
}

fn sensitive_header(value: &str, label: &str) -> Result<HeaderValue, LlmError> {
    let mut value = HeaderValue::from_str(value)
        .map_err(|_| invalid_config(format!("{label} contains invalid header characters.")))?;
    value.set_sensitive(true);
    Ok(value)
}

fn request_header(value: &str) -> Result<HeaderValue, LlmError> {
    HeaderValue::from_str(value)
        .map_err(|_| invalid_request("search request metadata contains invalid header characters"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn resolves_supported_base_url_shapes() {
        assert_eq!(
            responses_url("https://chatgpt.com/backend-api"),
            "https://chatgpt.com/backend-api/codex/responses"
        );
        assert_eq!(
            responses_url("https://example.com/codex/"),
            "https://example.com/codex/responses"
        );
        assert_eq!(
            search_url("https://chatgpt.com/backend-api"),
            "https://chatgpt.com/backend-api/codex/alpha/search"
        );
        assert_eq!(
            search_url("https://example.com/codex/responses"),
            "https://example.com/codex/alpha/search"
        );
    }
}
