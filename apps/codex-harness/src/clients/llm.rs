use std::{future::Future, time::Duration};

use execution_runtime::OperationContext;
use llm_contracts::{AssistantMessage, LlmError, LlmRequest};
use reqwest::StatusCode;
use serde::{Deserialize, Serialize};
use url::Url;
use uuid::Uuid;

use crate::config::LlmGatewayServiceConfig;

/// Thin non-streaming transport for the shared LLM gateway. Retry semantics
/// intentionally live in the Codex model client, not in this HTTP adapter.
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

    pub async fn complete(
        &self,
        account_id: Option<Uuid>,
        request: &LlmRequest,
        operation: &OperationContext,
    ) -> Result<AssistantMessage, LlmGatewayClientError> {
        let response = await_operation(
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
        let body = await_operation(operation, response.bytes()).await?;
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

#[derive(Debug, Deserialize, PartialEq)]
pub struct LlmGatewayError {
    pub kind: String,
    pub message: String,
    pub can_retry: bool,
    pub retry_after_ms: Option<u64>,
    pub provider_error: Option<Box<LlmError>>,
}

impl LlmGatewayError {
    #[must_use]
    pub fn retry_after(&self) -> Option<Duration> {
        self.retry_after_ms.map(Duration::from_millis)
    }
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

async fn await_operation<T, F>(
    operation: &OperationContext,
    future: F,
) -> Result<T, LlmGatewayClientError>
where
    F: Future<Output = Result<T, reqwest::Error>>,
{
    if operation.is_cancelled() {
        return Err(LlmGatewayClientError::Cancelled);
    }
    match operation.remaining() {
        Some(remaining) if remaining.is_zero() => Err(LlmGatewayClientError::DeadlineExceeded),
        Some(remaining) => {
            tokio::select! {
                biased;
                () = operation.cancelled() => Err(LlmGatewayClientError::Cancelled),
                () = tokio::time::sleep(remaining) => {
                    Err(LlmGatewayClientError::DeadlineExceeded)
                }
                result = future => result.map_err(LlmGatewayClientError::Request),
            }
        }
        None => {
            tokio::select! {
                biased;
                () = operation.cancelled() => Err(LlmGatewayClientError::Cancelled),
                result = future => result.map_err(LlmGatewayClientError::Request),
            }
        }
    }
}
