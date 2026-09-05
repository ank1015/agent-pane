use uuid::Uuid;

use crate::GatewayFailure;

pub type ClientResult<T> = Result<T, ClientError>;

#[derive(Debug, thiserror::Error)]
pub enum ClientError {
    #[error("invalid LLM request: {0}")]
    InvalidRequest(#[from] llm_contracts::ValidationError),
    #[error("LLM gateway transport failed")]
    Transport(#[source] reqwest::Error),
    #[error("LLM gateway rejected the request with HTTP {status}: {}", failure.error.message)]
    Gateway {
        status: u16,
        failure: Box<GatewayFailure>,
    },
    #[error("LLM gateway returned HTTP {status} without a structured error")]
    Http {
        status: u16,
        request_id: Option<String>,
    },
    #[error("invalid LLM gateway response: {0}")]
    Protocol(&'static str),
    #[error("LLM gateway response exceeded the configured byte limit")]
    ResponseTooLarge,
    #[error("LLM run {run_id} failed: {}", failure.error.message)]
    RunFailed {
        run_id: Uuid,
        failure: Box<GatewayFailure>,
    },
    #[error("LLM run {run_id} has expired")]
    RunExpired { run_id: Uuid },
    #[error("LLM run {run_id} was aborted")]
    RunAborted { run_id: Uuid },
    #[error("timed out waiting for LLM run {run_id}; the run may still be executing")]
    WaitTimeout { run_id: Uuid },
    #[error("could not continue waiting for LLM run {run_id}: {source}")]
    WaitInterrupted {
        run_id: Uuid,
        #[source]
        source: Box<ClientError>,
    },
}

impl ClientError {
    pub(crate) fn transport(error: reqwest::Error) -> Self {
        Self::Transport(error.without_url())
    }
}
