use std::{future::Future, time::Duration};

use execution_runtime::OperationContext;
use llm_contracts::{AssistantMessage, LlmError, LlmRequest};
use reqwest::{Response, StatusCode};
use serde::{Deserialize, Serialize};
use url::Url;
use uuid::Uuid;

use crate::config::LlmGatewayServiceConfig;

#[derive(Clone)]
pub struct LlmGatewayClient {
    http: reqwest::Client,
    complete_url: Url,
}

impl LlmGatewayClient {
    pub fn new(config: LlmGatewayServiceConfig) -> Result<Self, LlmGatewayClientError> {
        let complete_url = endpoint(&config.base_url, &["v1", "complete"])?;
        let http = reqwest::Client::builder()
            .timeout(config.request_timeout)
            .build()
            .map_err(LlmGatewayClientError::BuildClient)?;
        Ok(Self { http, complete_url })
    }

    /// Completes any LLM request through the gateway.
    ///
    /// `operation` is the process-local abort signal shared with the rest of the
    /// turn. Dropping the HTTP future on cancellation also closes the in-flight
    /// gateway request.
    pub async fn complete(
        &self,
        account_id: Option<Uuid>,
        request: &LlmRequest,
        operation: &OperationContext,
    ) -> Result<AssistantMessage, LlmGatewayClientError> {
        let response = await_response(
            operation,
            self.http
                .post(self.complete_url.clone())
                .json(&CompleteRequest {
                    account_id,
                    request,
                })
                .send(),
        )
        .await?;
        let status = response.status();
        let body = response
            .bytes()
            .await
            .map_err(LlmGatewayClientError::Request)?;
        if !status.is_success() {
            let error = serde_json::from_slice::<ErrorEnvelope>(&body)
                .ok()
                .map(|response| response.error);
            return Err(LlmGatewayClientError::Rejected {
                status,
                error,
                body: String::from_utf8_lossy(&body).into_owned(),
            });
        }
        let response = serde_json::from_slice::<CompleteResponse>(&body)
            .map_err(LlmGatewayClientError::InvalidResponse)?;
        Ok(response.message)
    }
}

impl LlmGatewayClientError {
    #[must_use]
    pub fn retryable(&self) -> bool {
        match self {
            Self::Request(_) => true,
            Self::Rejected {
                status,
                error: Some(error),
                ..
            } => error.can_retry || status.is_server_error(),
            Self::Rejected { status, .. } => status.is_server_error(),
            Self::InvalidUrl
            | Self::BuildClient(_)
            | Self::Cancelled
            | Self::DeadlineExceeded
            | Self::InvalidResponse(_) => false,
        }
    }

    #[must_use]
    pub fn retry_after(&self) -> Option<Duration> {
        let Self::Rejected {
            error: Some(error), ..
        } = self
        else {
            return None;
        };
        error.retry_after_ms.map(Duration::from_millis)
    }

    #[must_use]
    pub fn is_context_overflow(&self) -> bool {
        let Self::Rejected {
            error: Some(error), ..
        } = self
        else {
            return false;
        };
        let provider = error.provider_error.as_deref();
        if provider
            .and_then(|error| error.provider_code.as_deref())
            .is_some_and(|code| code.eq_ignore_ascii_case("context_length_exceeded"))
        {
            return true;
        }
        let message = provider.map_or(error.message.as_str(), |error| error.message.as_str());
        let message = message.to_ascii_lowercase();
        [
            "context length exceeded",
            "context window exceeded",
            "exceeds the context window",
            "maximum context length",
            "too many tokens",
        ]
        .iter()
        .any(|pattern| message.contains(pattern))
    }
}

#[derive(Debug, Deserialize, PartialEq)]
pub struct LlmGatewayError {
    pub kind: String,
    pub message: String,
    pub can_retry: bool,
    pub retry_after_ms: Option<u64>,
    pub provider_error: Option<Box<LlmError>>,
}

#[derive(Debug, thiserror::Error)]
pub enum LlmGatewayClientError {
    #[error("could not construct the LLM gateway completion URL")]
    InvalidUrl,
    #[error("could not build the LLM gateway HTTP client")]
    BuildClient(#[source] reqwest::Error),
    #[error("LLM gateway request failed")]
    Request(#[source] reqwest::Error),
    #[error("LLM request was cancelled")]
    Cancelled,
    #[error("LLM request deadline was exceeded")]
    DeadlineExceeded,
    #[error("LLM gateway rejected the request with status {status}")]
    Rejected {
        status: StatusCode,
        error: Option<LlmGatewayError>,
        body: String,
    },
    #[error("LLM gateway returned invalid JSON")]
    InvalidResponse(#[source] serde_json::Error),
}

#[derive(Serialize)]
struct CompleteRequest<'a> {
    account_id: Option<Uuid>,
    request: &'a LlmRequest,
}

#[derive(Deserialize)]
struct CompleteResponse {
    message: AssistantMessage,
}

#[derive(Deserialize)]
struct ErrorEnvelope {
    error: LlmGatewayError,
}

fn endpoint(base_url: &Url, segments: &[&str]) -> Result<Url, LlmGatewayClientError> {
    let mut url = base_url.clone();
    let mut path = url
        .path_segments_mut()
        .map_err(|()| LlmGatewayClientError::InvalidUrl)?;
    path.pop_if_empty();
    path.extend(segments.iter().copied());
    drop(path);
    Ok(url)
}

async fn await_response<F>(
    operation: &OperationContext,
    request: F,
) -> Result<Response, LlmGatewayClientError>
where
    F: Future<Output = Result<Response, reqwest::Error>>,
{
    if operation.is_cancelled() {
        return Err(LlmGatewayClientError::Cancelled);
    }
    match operation.remaining() {
        Some(remaining) => await_with_deadline(operation, remaining, request).await,
        None => {
            tokio::select! {
                biased;
                _ = operation.cancelled() => Err(LlmGatewayClientError::Cancelled),
                result = request => result.map_err(LlmGatewayClientError::Request),
            }
        }
    }
}

async fn await_with_deadline<F>(
    operation: &OperationContext,
    remaining: Duration,
    request: F,
) -> Result<Response, LlmGatewayClientError>
where
    F: Future<Output = Result<Response, reqwest::Error>>,
{
    if remaining.is_zero() {
        return Err(LlmGatewayClientError::DeadlineExceeded);
    }
    tokio::select! {
        biased;
        _ = operation.cancelled() => Err(LlmGatewayClientError::Cancelled),
        _ = tokio::time::sleep(remaining) => Err(LlmGatewayClientError::DeadlineExceeded),
        result = request => result.map_err(LlmGatewayClientError::Request),
    }
}
