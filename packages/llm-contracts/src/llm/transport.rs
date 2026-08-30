use async_trait::async_trait;

use crate::{
    AssistantMessage, LlmError, LlmRequest, ProviderId, SearchRequest, SearchRequestOptions,
    SearchResponse,
};

/// Completes an LLM request without exposing provider streaming details.
#[async_trait]
pub trait LlmTransport: Send + Sync {
    async fn complete(&self, request: LlmRequest) -> Result<AssistantMessage, LlmError>;

    /// Executes the Codex provider-backed web search extension when supported.
    async fn search(
        &self,
        _request: SearchRequest,
        _options: SearchRequestOptions,
    ) -> Result<SearchResponse, LlmError> {
        Err(LlmError {
            message: "provider does not support the alpha/search API".to_owned(),
            provider_code: None,
            provider_type: Some("unsupported_operation".to_owned()),
            http_status: None,
            can_retry: false,
            retry_after_ms: None,
            native_error: None,
        })
    }
}

/// Provider-specific implementation usable through the common transport API.
pub trait LlmProviderAdapter: LlmTransport {
    fn provider(&self) -> &ProviderId;
}
