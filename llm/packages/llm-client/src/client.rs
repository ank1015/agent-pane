use std::{sync::Arc, time::Duration};

use llm_contracts::Validate;
use reqwest::{
    Client, StatusCode,
    header::{ACCEPT, AUTHORIZATION, HeaderValue},
};
use url::Url;
use uuid::Uuid;

use crate::{
    ClientError, ClientResult, CompletionRequest, CompletionResponse, ConfigError, GatewayFailure,
    IdempotencyKey, LlmClientConfig, Run, RunState, WaitOptions,
};

/// Authenticated, cheaply cloneable client for gateway-managed LLM runs.
#[derive(Clone, Debug)]
pub struct LlmClient {
    inner: Arc<Inner>,
}

#[derive(Debug)]
struct Inner {
    http: Client,
    runs_url: Url,
    authorization: HeaderValue,
    max_response_bytes: usize,
    wait_seconds: u64,
}

impl LlmClient {
    pub fn new(config: LlmClientConfig) -> Result<Self, ConfigError> {
        let authorization = config.authorization()?;
        let http = Client::builder()
            .timeout(config.request_timeout)
            .redirect(reqwest::redirect::Policy::none())
            .retry(reqwest::retry::never())
            .build()
            .map_err(ConfigError::BuildClient)?;
        let mut runs_url = config.base_url;
        runs_url
            .path_segments_mut()
            .map_err(|_| ConfigError::Invalid("base URL cannot contain path segments"))?
            .pop_if_empty()
            .extend(["v1", "llm", "runs"]);
        Ok(Self {
            inner: Arc::new(Inner {
                http,
                runs_url,
                authorization,
                max_response_bytes: config.max_response_bytes,
                // Leave room for HTTP overhead; short request timeouts use immediate reads.
                wait_seconds: config.request_timeout.as_secs().saturating_sub(5).min(25),
            }),
        })
    }

    /// Start or recover a run with a caller-owned, persisted idempotency key.
    /// No automatic retries. A failed submission response may have been accepted;
    /// explicitly resubmit the same key and request to recover its run ID.
    pub async fn submit(
        &self,
        key: &IdempotencyKey,
        request: &CompletionRequest,
    ) -> ClientResult<Run> {
        request.request.validate()?;
        let builder = self
            .inner
            .http
            .post(self.inner.runs_url.clone())
            .header("Idempotency-Key", key.as_str())
            .json(request);
        self.send_run(builder, None).await
    }

    /// Retrieve a snapshot immediately, including failed or expired runs.
    pub async fn get_run(&self, run_id: Uuid) -> ClientResult<Run> {
        self.retrieve(run_id, 0).await
    }

    /// Wait for a previously submitted run, including after a caller restart.
    /// Dropping this future or reaching its timeout does not cancel the remote run.
    pub async fn wait(
        &self,
        run_id: Uuid,
        options: WaitOptions,
    ) -> ClientResult<CompletionResponse> {
        if options.timeout.is_zero() {
            return Err(ClientError::WaitTimeout { run_id });
        }
        tokio::time::timeout(options.timeout, self.wait_loop(run_id))
            .await
            .map_err(|_| ClientError::WaitTimeout { run_id })?
    }

    /// Submit, then await the same run. The timeout bounds waiting after acceptance.
    /// Errors after acceptance retain the run ID so callers can resume waiting.
    pub async fn complete(
        &self,
        key: &IdempotencyKey,
        request: &CompletionRequest,
        options: WaitOptions,
    ) -> ClientResult<CompletionResponse> {
        let run = self.submit(key, request).await?;
        let run_id = run.run_id;
        match terminal_result(run)? {
            Some(result) => Ok(result),
            None => self.wait(run_id, options).await,
        }
    }

    async fn wait_loop(&self, run_id: Uuid) -> ClientResult<CompletionResponse> {
        loop {
            let started = tokio::time::Instant::now();
            let run = self
                .retrieve(run_id, self.inner.wait_seconds)
                .await
                .map_err(|source| ClientError::WaitInterrupted {
                    run_id,
                    source: Box::new(source),
                })?;
            if let Some(result) = terminal_result(run)? {
                return Ok(result);
            }
            // A gateway may return Running before the requested wait elapsed.
            // Avoid busy polling, while adding no delay after a completed long poll.
            tokio::time::sleep(Duration::from_secs(1).saturating_sub(started.elapsed())).await;
        }
    }

    async fn retrieve(&self, run_id: Uuid, wait_seconds: u64) -> ClientResult<Run> {
        let mut url = self.inner.runs_url.clone();
        url.path_segments_mut()
            .expect("validated gateway URL")
            .push(&run_id.to_string());
        let builder = self
            .inner
            .http
            .get(url)
            .query(&[("wait_seconds", wait_seconds)]);
        self.send_run(builder, Some(run_id)).await
    }

    async fn send_run(
        &self,
        builder: reqwest::RequestBuilder,
        expected_id: Option<Uuid>,
    ) -> ClientResult<Run> {
        let mut response = builder
            .header(AUTHORIZATION, self.inner.authorization.clone())
            .header(ACCEPT, "application/json")
            .send()
            .await
            .map_err(ClientError::transport)?;
        let status = response.status();
        let request_id = response
            .headers()
            .get("x-request-id")
            .and_then(|v| v.to_str().ok())
            .map(str::to_owned);
        let limit = self.inner.max_response_bytes;
        if response
            .content_length()
            .is_some_and(|length| length > limit as u64)
        {
            return Err(ClientError::ResponseTooLarge);
        }
        let mut bytes = Vec::new();
        while let Some(chunk) = response.chunk().await.map_err(ClientError::transport)? {
            if chunk.len() > limit.saturating_sub(bytes.len()) {
                return Err(ClientError::ResponseTooLarge);
            }
            bytes.extend_from_slice(&chunk);
        }
        if !matches!(
            status,
            StatusCode::OK | StatusCode::ACCEPTED | StatusCode::GONE
        ) {
            return Err(match serde_json::from_slice::<GatewayFailure>(&bytes) {
                Ok(failure) => ClientError::Gateway {
                    status: status.as_u16(),
                    failure: Box::new(failure),
                },
                Err(_) => ClientError::Http {
                    status: status.as_u16(),
                    request_id,
                },
            });
        }
        let run: Run = serde_json::from_slice(&bytes)
            .map_err(|_| ClientError::Protocol("invalid run JSON or result shape"))?;
        if expected_id.is_some_and(|id| id != run.run_id) {
            return Err(ClientError::Protocol(
                "run identity differs from the requested run",
            ));
        }
        match &run.state {
            RunState::Succeeded(result) if result.request_id != run.run_id => {
                return Err(ClientError::Protocol(
                    "completion identity differs from its run",
                ));
            }
            RunState::Failed(result) if result.request_id != run.run_id => {
                return Err(ClientError::Protocol(
                    "failure identity differs from its run",
                ));
            }
            _ => {}
        }
        if (status == StatusCode::GONE) != matches!(run.state, RunState::Expired)
            || (status == StatusCode::ACCEPTED && !matches!(run.state, RunState::Running))
        {
            return Err(ClientError::Protocol(
                "HTTP status disagrees with run state",
            ));
        }
        Ok(run)
    }
}

fn terminal_result(run: Run) -> ClientResult<Option<CompletionResponse>> {
    match run.state {
        RunState::Running => Ok(None),
        RunState::Succeeded(result) => Ok(Some(*result)),
        RunState::Failed(failure) => Err(ClientError::RunFailed {
            run_id: run.run_id,
            failure: Box::new(failure),
        }),
        RunState::Expired => Err(ClientError::RunExpired { run_id: run.run_id }),
    }
}
