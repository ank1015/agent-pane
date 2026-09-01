use agent_harness_sdk::ActiveTurnError;
use execution_gateway_client::ExecutionGatewayClientError;

use crate::{
    clients::LlmGatewayClientError,
    harness::{
        context_formation::ContextFormationError, model_resolver::ModelResolverError,
        tools::ToolExecutionError,
    },
};

use super::{config::EnvironmentHarnessConfigError, transcript::TranscriptError};

#[derive(Debug, thiserror::Error)]
pub enum EnvironmentRuntimeBuildError {
    #[error(transparent)]
    Llm(#[from] LlmGatewayClientError),
    #[error(transparent)]
    Execution(#[from] ExecutionGatewayClientError),
    #[error("could not configure the search tool: {0}")]
    SearchTool(#[source] tool_firecrawl_search::FirecrawlSearchToolError),
    #[error("could not configure the scrape tool: {0}")]
    ScrapeTool(#[source] tool_firecrawl_scrape::FirecrawlScrapeToolError),
}

#[derive(Debug, thiserror::Error)]
pub enum EnvironmentRuntimeError {
    #[error("Agent transcript access remained unavailable after retries")]
    Agent(#[source] ActiveTurnError),
}

#[derive(Debug, thiserror::Error)]
pub(super) enum TurnError {
    #[error(transparent)]
    Config(#[from] EnvironmentHarnessConfigError),
    #[error(transparent)]
    Model(#[from] ModelResolverError),
    #[error(transparent)]
    Context(#[from] ContextFormationError),
    #[error(transparent)]
    Llm(#[from] LlmGatewayClientError),
    #[error(transparent)]
    Agent(#[from] ActiveTurnError),
    #[error(transparent)]
    Tool(#[from] ToolExecutionError),
    #[error(transparent)]
    Transcript(#[from] TranscriptError),
    #[error("could not serialize the compaction message")]
    CompactionMessage(#[from] serde_json::Error),
    #[error("compaction could not find a message boundary to retain")]
    NoCompactionCutPoint,
    #[error("the compaction model returned an empty summary")]
    EmptyCompactionSummary,
    #[error("expected an assistant tool call")]
    ExpectedToolCall,
    #[error("the active run was cancelled")]
    Cancelled,
}

impl TurnError {
    pub(super) fn code(&self) -> &'static str {
        match self {
            Self::Config(_) => "invalid_harness_config",
            Self::Model(_) => "unsupported_model_config",
            Self::Context(_) => "context_formation_failed",
            Self::Llm(_) => "llm_request_failed",
            Self::Agent(_) => "agent_request_failed",
            Self::Tool(_) => "tool_execution_failed",
            Self::Transcript(_) => "invalid_turn_transcript",
            Self::CompactionMessage(_)
            | Self::NoCompactionCutPoint
            | Self::EmptyCompactionSummary => "compaction_failed",
            Self::ExpectedToolCall => "invalid_tool_call",
            Self::Cancelled => "cancelled",
        }
    }

    pub(super) fn agent_retryable(&self) -> bool {
        matches!(self, Self::Agent(error) if error.retryable())
    }

    pub(super) fn agent_stale(&self) -> bool {
        matches!(self, Self::Agent(error) if error.stale())
    }
}
