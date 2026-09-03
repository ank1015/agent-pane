use async_trait::async_trait;

use crate::{
    AssistantMessage, LlmError, LlmRequest, ProviderId, SearchRequest, SearchRequestOptions,
    SearchResponse,
};

/// Provider transport for model completion.
#[async_trait]
pub trait LlmTransport: Send + Sync {
    async fn complete(&self, request: LlmRequest) -> Result<AssistantMessage, LlmError>;
}

/// Optional provider transport for the Codex provider-backed search API.
#[async_trait]
pub trait SearchTransport: Send + Sync {
    async fn search(
        &self,
        request: SearchRequest,
        options: SearchRequestOptions,
    ) -> Result<SearchResponse, LlmError>;
}

/// Provider-specific completion implementation usable through the common API.
pub trait LlmProviderAdapter: LlmTransport {
    fn provider(&self) -> &ProviderId;
}
