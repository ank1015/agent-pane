use async_trait::async_trait;

use crate::{AssistantMessage, LlmError, LlmRequest, ProviderId};

/// Completes an LLM request without exposing provider streaming details.
#[async_trait]
pub trait LlmTransport: Send + Sync {
    async fn complete(&self, request: LlmRequest) -> Result<AssistantMessage, LlmError>;
}

/// Provider-specific implementation usable through the common transport API.
pub trait LlmProviderAdapter: LlmTransport {
    fn provider(&self) -> &ProviderId;
}
