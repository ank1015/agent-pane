use execution_gateway_client::ExecutionGatewayClientError;

use crate::{
    clients::LlmGatewayClientError,
    harness::{
        context_formation::ContextFormationError, model_resolver::ModelResolverError,
        tools::ToolExecutionError,
    },
    server::ActiveTurnError,
};

use super::{config::PiHarnessConfigError, transcript::TranscriptError};

#[derive(Debug, thiserror::Error)]
pub enum PiRuntimeBuildError {
    #[error(transparent)]
    Llm(#[from] LlmGatewayClientError),
    #[error(transparent)]
    Execution(#[from] ExecutionGatewayClientError),
}

#[derive(Debug, thiserror::Error)]
pub enum PiRuntimeError {
    #[error("Agent transcript access remained unavailable after retries")]
    Agent(#[source] ActiveTurnError),
}

#[derive(Debug, thiserror::Error)]
pub(super) enum TurnError {
    #[error(transparent)]
    Config(#[from] PiHarnessConfigError),
    #[error(transparent)]
    Model(#[from] ModelResolverError),
    #[error(transparent)]
    Context(#[from] ContextFormationError),
    #[error(transparent)]
    Llm(#[from] LlmGatewayClientError),
    #[error(transparent)]
    Agent(#[from] ActiveTurnError),
    #[error(transparent)]
    Execution(#[from] ExecutionGatewayClientError),
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
    #[error("workspace root {0:?} is not exposed by the selected machine")]
    WorkspaceRootNotFound(String),
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
            Self::Execution(_) => "execution_runtime_failed",
            Self::Tool(_) => "tool_execution_failed",
            Self::Transcript(_) => "invalid_turn_transcript",
            Self::CompactionMessage(_)
            | Self::NoCompactionCutPoint
            | Self::EmptyCompactionSummary => "compaction_failed",
            Self::WorkspaceRootNotFound(_) => "invalid_workspace_config",
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
