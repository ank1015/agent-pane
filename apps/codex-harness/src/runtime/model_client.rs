use std::{sync::Arc, time::Duration};

use execution_runtime::OperationContext;
use llm_contracts::{AssistantMessage, LlmError, LlmRequest, Validate, ValidationError};
use rand::Rng;
use reqwest::StatusCode;
use uuid::Uuid;

use crate::{
    clients::{LlmGatewayClient, LlmGatewayClientError, LlmGatewayError},
    config::LlmGatewayServiceConfig,
};

const CODEX_REQUEST_MAX_RETRIES: u32 = 4;
const CODEX_RESPONSE_MAX_RETRIES: u32 = 5;
const CODEX_RETRY_BASE_DELAY: Duration = Duration::from_millis(200);

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct CodexRetryPolicy {
    pub request_max_retries: u32,
    pub response_max_retries: u32,
    pub base_delay: Duration,
}

impl CodexRetryPolicy {
    #[must_use]
    pub const fn codex_default() -> Self {
        Self {
            request_max_retries: CODEX_REQUEST_MAX_RETRIES,
            response_max_retries: CODEX_RESPONSE_MAX_RETRIES,
            base_delay: CODEX_RETRY_BASE_DELAY,
        }
    }

    fn delay(self, retry: u32) -> Duration {
        let jitter = rand::thread_rng().gen_range(0.9..1.1);
        self.delay_with_jitter(retry, jitter)
    }

    fn delay_with_jitter(self, retry: u32, jitter: f64) -> Duration {
        let exponent = retry.saturating_sub(1);
        let multiplier = 1_u32.checked_shl(exponent).unwrap_or(u32::MAX);
        self.base_delay.mul_f64(f64::from(multiplier) * jitter)
    }
}

impl Default for CodexRetryPolicy {
    fn default() -> Self {
        Self::codex_default()
    }
}

#[async_trait::async_trait]
trait ModelGateway: Send + Sync {
    async fn complete(
        &self,
        account_id: Option<Uuid>,
        request: &LlmRequest,
        operation: &OperationContext,
    ) -> Result<AssistantMessage, LlmGatewayClientError>;
}

#[async_trait::async_trait]
impl ModelGateway for LlmGatewayClient {
    async fn complete(
        &self,
        account_id: Option<Uuid>,
        request: &LlmRequest,
        operation: &OperationContext,
    ) -> Result<AssistantMessage, LlmGatewayClientError> {
        self.complete(account_id, request, operation).await
    }
}

/// Non-streaming Codex model client. The gateway is only a transport boundary;
/// retry classification and backoff mirror Codex's request and response layers.
#[derive(Clone)]
pub struct CodexModelClient {
    gateway: Arc<dyn ModelGateway>,
    retry_policy: CodexRetryPolicy,
}

impl CodexModelClient {
    pub fn from_config(config: LlmGatewayServiceConfig) -> Result<Self, LlmGatewayClientError> {
        Ok(Self::new(LlmGatewayClient::new(config)?))
    }

    #[must_use]
    pub fn new(gateway: LlmGatewayClient) -> Self {
        Self {
            gateway: Arc::new(gateway),
            retry_policy: CodexRetryPolicy::default(),
        }
    }

    #[must_use]
    pub fn with_retry_policy(mut self, retry_policy: CodexRetryPolicy) -> Self {
        self.retry_policy = retry_policy;
        self
    }

    pub async fn complete(
        &self,
        account_id: Option<Uuid>,
        request: &LlmRequest,
        operation: &OperationContext,
    ) -> Result<AssistantMessage, CodexModelCallError> {
        if let Err(error) = request.validate() {
            return Err(CodexModelCallError::new(
                ModelAttemptError::InvalidRequest(error),
                0,
                false,
            ));
        }

        let mut attempts = 0_u32;
        let mut request_retries = 0_u32;
        let mut response_retries = 0_u32;
        loop {
            attempts = attempts.saturating_add(1);
            let attempt = self
                .gateway
                .complete(account_id, request, operation)
                .await
                .map_err(ModelAttemptError::Gateway)
                .and_then(|message| {
                    message
                        .validate()
                        .map(|()| message)
                        .map_err(ModelAttemptError::InvalidAssistant)
                });
            let error = match attempt {
                Ok(message) => return Ok(message),
                Err(error) => error,
            };
            let retry_class = retry_class(&error);
            let retry = match retry_class {
                RetryClass::Never => None,
                RetryClass::Request if request_retries < self.retry_policy.request_max_retries => {
                    request_retries = request_retries.saturating_add(1);
                    Some(request_retries)
                }
                RetryClass::Response
                    if response_retries < self.retry_policy.response_max_retries =>
                {
                    response_retries = response_retries.saturating_add(1);
                    Some(response_retries)
                }
                RetryClass::Request | RetryClass::Response => None,
            };
            let Some(retry) = retry else {
                return Err(CodexModelCallError::new(
                    error,
                    attempts,
                    retry_class != RetryClass::Never,
                ));
            };
            let local_delay = self.retry_policy.delay(retry);
            let delay = if retry_class == RetryClass::Response {
                response_retry_after(&error).unwrap_or(local_delay)
            } else {
                // Codex's Responses HTTP retry layer currently ignores
                // Retry-After and uses its local jittered backoff.
                local_delay
            };
            tracing::warn!(
                retry,
                request_retries,
                response_retries,
                delay_ms = delay.as_millis(),
                error = %error,
                "Codex model request failed; retrying"
            );
            if let Err(error) = sleep_with_operation(operation, delay).await {
                return Err(CodexModelCallError::new(
                    ModelAttemptError::Gateway(error),
                    attempts,
                    false,
                ));
            }
        }
    }

    #[cfg(test)]
    fn from_gateway(gateway: Arc<dyn ModelGateway>, retry_policy: CodexRetryPolicy) -> Self {
        Self {
            gateway,
            retry_policy,
        }
    }
}

#[async_trait::async_trait]
pub trait CodexCompletionClient: Send + Sync {
    async fn complete(
        &self,
        account_id: Option<Uuid>,
        request: &LlmRequest,
        operation: &OperationContext,
    ) -> Result<AssistantMessage, CodexModelCallError>;
}

#[async_trait::async_trait]
impl CodexCompletionClient for CodexModelClient {
    async fn complete(
        &self,
        account_id: Option<Uuid>,
        request: &LlmRequest,
        operation: &OperationContext,
    ) -> Result<AssistantMessage, CodexModelCallError> {
        CodexModelClient::complete(self, account_id, request, operation).await
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum CodexModelFailureKind {
    Cancelled,
    DeadlineExceeded,
    ContextWindowExceeded,
    UsageLimitReached,
    ServerOverloaded,
    Authentication,
    InvalidRequest,
    InvalidResponse,
    Configuration,
    Transport,
    Provider,
}

#[derive(Debug, thiserror::Error)]
#[error("Codex model call failed after {attempts} attempt(s): {source}")]
pub struct CodexModelCallError {
    pub kind: CodexModelFailureKind,
    pub attempts: u32,
    pub retry_exhausted: bool,
    #[source]
    source: ModelAttemptError,
}

impl CodexModelCallError {
    fn new(source: ModelAttemptError, attempts: u32, retry_exhausted: bool) -> Self {
        Self {
            kind: failure_kind(&source),
            attempts,
            retry_exhausted,
            source,
        }
    }

    #[must_use]
    pub const fn attempt_error(&self) -> &ModelAttemptError {
        &self.source
    }

    #[must_use]
    pub fn retry_after(&self) -> Option<Duration> {
        response_retry_after(&self.source)
    }
}

#[derive(Debug, thiserror::Error)]
pub enum ModelAttemptError {
    #[error("formed LLM request is invalid")]
    InvalidRequest(#[source] ValidationError),
    #[error("LLM gateway call failed")]
    Gateway(#[source] LlmGatewayClientError),
    #[error("LLM gateway returned an invalid assistant message")]
    InvalidAssistant(#[source] ValidationError),
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum RetryClass {
    Never,
    Request,
    Response,
}

fn retry_class(error: &ModelAttemptError) -> RetryClass {
    match error {
        ModelAttemptError::InvalidRequest(_) => RetryClass::Never,
        ModelAttemptError::InvalidAssistant(_) => RetryClass::Response,
        ModelAttemptError::Gateway(LlmGatewayClientError::Request(_)) => RetryClass::Request,
        ModelAttemptError::Gateway(LlmGatewayClientError::InvalidResponse(_)) => {
            RetryClass::Response
        }
        ModelAttemptError::Gateway(
            LlmGatewayClientError::InvalidUrl
            | LlmGatewayClientError::BuildClient(_)
            | LlmGatewayClientError::Cancelled
            | LlmGatewayClientError::DeadlineExceeded,
        ) => RetryClass::Never,
        ModelAttemptError::Gateway(LlmGatewayClientError::Rejected { status, error, .. }) => {
            rejected_retry_class(*status, error.as_ref())
        }
    }
}

fn rejected_retry_class(status: StatusCode, error: Option<&LlmGatewayError>) -> RetryClass {
    let Some(error) = error else {
        return if status.is_server_error() {
            RetryClass::Request
        } else {
            RetryClass::Never
        };
    };
    match error.kind.as_str() {
        "overloaded" | "provider_timeout" | "internal" => RetryClass::Request,
        "provider" => error.provider_error.as_deref().map_or_else(
            || {
                if error.can_retry {
                    RetryClass::Response
                } else {
                    RetryClass::Never
                }
            },
            provider_retry_class,
        ),
        _ => RetryClass::Never,
    }
}

fn provider_retry_class(error: &LlmError) -> RetryClass {
    if is_context_window(error)
        || is_usage_limit(error)
        || is_authentication(error)
        || is_invalid_request(error)
    {
        return RetryClass::Never;
    }
    // Codex retries raw 5xx Responses HTTP failures before interpreting their
    // semantic body. An overload delivered as response.failed has no HTTP
    // status and remains a terminal semantic failure below.
    if error.http_status.is_some_and(|status| status >= 500) || is_transport(error) {
        return RetryClass::Request;
    }
    if is_explicit_server_overload(error) {
        return RetryClass::Never;
    }
    if field_is(
        error.provider_type.as_deref(),
        &["response_failed", "invalid_response"],
    ) || (error.http_status.is_none() && error.can_retry)
    {
        return RetryClass::Response;
    }
    RetryClass::Never
}

fn failure_kind(error: &ModelAttemptError) -> CodexModelFailureKind {
    match error {
        ModelAttemptError::InvalidRequest(_) => CodexModelFailureKind::InvalidRequest,
        ModelAttemptError::InvalidAssistant(_) => CodexModelFailureKind::InvalidResponse,
        ModelAttemptError::Gateway(error) => gateway_failure_kind(error),
    }
}

fn gateway_failure_kind(error: &LlmGatewayClientError) -> CodexModelFailureKind {
    match error {
        LlmGatewayClientError::InvalidUrl | LlmGatewayClientError::BuildClient(_) => {
            CodexModelFailureKind::Configuration
        }
        LlmGatewayClientError::Request(_) => CodexModelFailureKind::Transport,
        LlmGatewayClientError::Cancelled => CodexModelFailureKind::Cancelled,
        LlmGatewayClientError::DeadlineExceeded => CodexModelFailureKind::DeadlineExceeded,
        LlmGatewayClientError::InvalidResponse(_) => CodexModelFailureKind::InvalidResponse,
        LlmGatewayClientError::Rejected { error, .. } => {
            let Some(error) = error else {
                return CodexModelFailureKind::Transport;
            };
            match error.kind.as_str() {
                "unauthorized"
                | "account_not_found"
                | "account_disabled"
                | "account_provider_mismatch"
                | "default_account_not_found"
                | "authentication_required" => CodexModelFailureKind::Authentication,
                "invalid_request" | "unknown_model" | "conflict" => {
                    CodexModelFailureKind::InvalidRequest
                }
                "overloaded" => CodexModelFailureKind::ServerOverloaded,
                "provider_timeout" | "internal" => CodexModelFailureKind::Transport,
                "provider" => error
                    .provider_error
                    .as_deref()
                    .map_or(CodexModelFailureKind::Provider, provider_failure_kind),
                _ => CodexModelFailureKind::Provider,
            }
        }
    }
}

fn provider_failure_kind(error: &LlmError) -> CodexModelFailureKind {
    if is_context_window(error) {
        CodexModelFailureKind::ContextWindowExceeded
    } else if is_usage_limit(error) {
        CodexModelFailureKind::UsageLimitReached
    } else if is_explicit_server_overload(error) {
        CodexModelFailureKind::ServerOverloaded
    } else if is_authentication(error) {
        CodexModelFailureKind::Authentication
    } else if is_invalid_request(error) {
        CodexModelFailureKind::InvalidRequest
    } else if field_is(error.provider_type.as_deref(), &["invalid_response"]) {
        CodexModelFailureKind::InvalidResponse
    } else if is_transport(error) {
        CodexModelFailureKind::Transport
    } else {
        CodexModelFailureKind::Provider
    }
}

fn is_context_window(error: &LlmError) -> bool {
    field_is(
        error.provider_code.as_deref(),
        &["context_length_exceeded", "context_window_exceeded"],
    ) || field_is(
        error.provider_type.as_deref(),
        &["context_length_exceeded", "context_window_exceeded"],
    )
}

fn is_usage_limit(error: &LlmError) -> bool {
    error.http_status == Some(429)
        || field_is(
            error.provider_code.as_deref(),
            &[
                "insufficient_quota",
                "quota_exceeded",
                "usage_limit_reached",
                "usage_not_included",
            ],
        )
        || field_is(
            error.provider_type.as_deref(),
            &[
                "usage_limit_reached",
                "usage_not_included",
                "quota_exceeded",
            ],
        )
}

fn is_explicit_server_overload(error: &LlmError) -> bool {
    field_is(
        error.provider_code.as_deref(),
        &["server_is_overloaded", "slow_down"],
    ) || field_is(error.provider_type.as_deref(), &["server_overloaded"])
}

fn is_authentication(error: &LlmError) -> bool {
    matches!(error.http_status, Some(401 | 403))
        || field_is(
            error.provider_type.as_deref(),
            &["authentication_error", "invalid_api_key"],
        )
}

fn is_invalid_request(error: &LlmError) -> bool {
    error.http_status == Some(400)
        || field_is(
            error.provider_type.as_deref(),
            &["invalid_request", "invalid_model", "invalid_config"],
        )
        || field_is(
            error.provider_code.as_deref(),
            &[
                "invalid_prompt",
                "bio_policy",
                "misalignment_policy_violation",
            ],
        )
}

fn is_transport(error: &LlmError) -> bool {
    field_is(
        error.provider_type.as_deref(),
        &["timeout", "connection_error", "network_error"],
    )
}

fn field_is(value: Option<&str>, expected: &[&str]) -> bool {
    value.is_some_and(|value| {
        expected
            .iter()
            .any(|expected| value.eq_ignore_ascii_case(expected))
    })
}

fn response_retry_after(error: &ModelAttemptError) -> Option<Duration> {
    let ModelAttemptError::Gateway(LlmGatewayClientError::Rejected {
        error: Some(error), ..
    }) = error
    else {
        return None;
    };
    error
        .provider_error
        .as_deref()
        .and_then(|provider| provider.retry_after_ms)
        .or(error.retry_after_ms)
        .map(Duration::from_millis)
}

async fn sleep_with_operation(
    operation: &OperationContext,
    delay: Duration,
) -> Result<(), LlmGatewayClientError> {
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
                () = tokio::time::sleep(delay) => Ok(()),
            }
        }
        None => {
            tokio::select! {
                biased;
                () = operation.cancelled() => Err(LlmGatewayClientError::Cancelled),
                () = tokio::time::sleep(delay) => Ok(()),
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use std::{
        collections::{BTreeMap, VecDeque},
        sync::{
            Arc, Mutex,
            atomic::{AtomicU32, Ordering},
        },
        time::Duration,
    };

    use llm_contracts::{
        AssistantContent, AssistantMessage, LlmRequest, MessageId, ModelId, ModelRef, ProviderId,
        StopReason, TextContent, Timestamp,
    };
    use reqwest::StatusCode;
    use serde_json::{Map, json};

    use super::*;

    #[test]
    fn uses_codex_retry_defaults_and_jitter_band() {
        let policy = CodexRetryPolicy::codex_default();
        assert_eq!(policy.request_max_retries, 4);
        assert_eq!(policy.response_max_retries, 5);
        assert_eq!(policy.base_delay, Duration::from_millis(200));
        assert_eq!(policy.delay_with_jitter(1, 0.9), Duration::from_millis(180));
        assert_eq!(
            policy.delay_with_jitter(4, 1.1),
            Duration::from_millis(1_760)
        );
    }

    #[tokio::test]
    async fn retries_request_failures_four_times_then_succeeds() {
        let gateway = FakeGateway::new(
            (0..4)
                .map(|_| Err(gateway_error("provider_timeout", None, true)))
                .chain(std::iter::once(Ok(assistant())))
                .collect(),
        );
        let client = test_client(&gateway);

        let message = client
            .complete(None, &request(), &OperationContext::new())
            .await
            .expect("fifth request succeeds");

        assert_eq!(message.id.as_str(), "assistant-1");
        assert_eq!(gateway.calls(), 5);
    }

    #[tokio::test]
    async fn retries_unknown_response_failures_on_the_response_budget() {
        let provider = provider_error(None, Some("response_failed"), None, false);
        let gateway = FakeGateway::new(VecDeque::from([
            Err(gateway_error("provider", Some(provider), false)),
            Ok(assistant()),
        ]));
        let client = test_client(&gateway);

        client
            .complete(None, &request(), &OperationContext::new())
            .await
            .expect("response retry succeeds");

        assert_eq!(gateway.calls(), 2);
    }

    #[tokio::test]
    async fn reports_response_retry_exhaustion_after_five_retries() {
        let responses = (0..6)
            .map(|_| {
                Err(gateway_error(
                    "provider",
                    Some(provider_error(None, Some("response_failed"), None, false)),
                    false,
                ))
            })
            .collect();
        let gateway = FakeGateway::new(responses);

        let error = test_client(&gateway)
            .complete(None, &request(), &OperationContext::new())
            .await
            .expect_err("response retry budget is exhausted");

        assert_eq!(error.kind, CodexModelFailureKind::Provider);
        assert_eq!(error.attempts, 6);
        assert!(error.retry_exhausted);
        assert_eq!(gateway.calls(), 6);
    }

    #[tokio::test]
    async fn does_not_retry_context_usage_or_explicit_overload_errors() {
        let cases = [
            (
                provider_error(
                    Some("context_length_exceeded"),
                    Some("response_failed"),
                    None,
                    false,
                ),
                CodexModelFailureKind::ContextWindowExceeded,
            ),
            (
                provider_error(None, Some("usage_limit_reached"), Some(429), true),
                CodexModelFailureKind::UsageLimitReached,
            ),
            (
                provider_error(
                    Some("server_is_overloaded"),
                    Some("response_failed"),
                    None,
                    true,
                ),
                CodexModelFailureKind::ServerOverloaded,
            ),
        ];
        for (provider, expected_kind) in cases {
            let gateway = FakeGateway::new(VecDeque::from([Err(gateway_error(
                "provider",
                Some(provider),
                true,
            ))]));
            let error = test_client(&gateway)
                .complete(None, &request(), &OperationContext::new())
                .await
                .expect_err("semantic error");
            assert_eq!(error.kind, expected_kind);
            assert_eq!(error.attempts, 1);
            assert!(!error.retry_exhausted);
            assert_eq!(gateway.calls(), 1);
        }
    }

    #[tokio::test]
    async fn cancellation_interrupts_retry_backoff() {
        let gateway = FakeGateway::new(VecDeque::from([Err(gateway_error(
            "provider_timeout",
            None,
            true,
        ))]));
        let client = CodexModelClient::from_gateway(
            Arc::new(gateway.clone()),
            CodexRetryPolicy {
                request_max_retries: 4,
                response_max_retries: 5,
                base_delay: Duration::from_secs(60),
            },
        );
        let operation = OperationContext::new();
        let cancellation = operation.clone();
        let task = tokio::spawn(async move { client.complete(None, &request(), &operation).await });
        while gateway.calls() == 0 {
            tokio::task::yield_now().await;
        }
        cancellation.cancel();

        let error = task.await.expect("task joins").expect_err("cancelled");
        assert_eq!(error.kind, CodexModelFailureKind::Cancelled);
        assert_eq!(error.attempts, 1);
    }

    fn test_client(gateway: &FakeGateway) -> CodexModelClient {
        CodexModelClient::from_gateway(
            Arc::new(gateway.clone()),
            CodexRetryPolicy {
                request_max_retries: 4,
                response_max_retries: 5,
                base_delay: Duration::ZERO,
            },
        )
    }

    #[derive(Clone)]
    struct FakeGateway {
        responses: Arc<Mutex<VecDeque<Result<AssistantMessage, LlmGatewayClientError>>>>,
        calls: Arc<AtomicU32>,
    }

    impl FakeGateway {
        fn new(responses: VecDeque<Result<AssistantMessage, LlmGatewayClientError>>) -> Self {
            Self {
                responses: Arc::new(Mutex::new(responses)),
                calls: Arc::new(AtomicU32::new(0)),
            }
        }

        fn calls(&self) -> u32 {
            self.calls.load(Ordering::SeqCst)
        }
    }

    #[async_trait::async_trait]
    impl ModelGateway for FakeGateway {
        async fn complete(
            &self,
            _account_id: Option<Uuid>,
            _request: &LlmRequest,
            _operation: &OperationContext,
        ) -> Result<AssistantMessage, LlmGatewayClientError> {
            self.calls.fetch_add(1, Ordering::SeqCst);
            self.responses
                .lock()
                .expect("responses lock")
                .pop_front()
                .expect("fake response")
        }
    }

    fn gateway_error(
        kind: &str,
        provider_error: Option<LlmError>,
        can_retry: bool,
    ) -> LlmGatewayClientError {
        LlmGatewayClientError::Rejected {
            status: if can_retry {
                StatusCode::SERVICE_UNAVAILABLE
            } else {
                StatusCode::BAD_GATEWAY
            },
            error: Some(LlmGatewayError {
                kind: kind.to_owned(),
                message: "failed".to_owned(),
                can_retry,
                retry_after_ms: None,
                provider_error: provider_error.map(Box::new),
            }),
            body: "{}".to_owned(),
        }
    }

    fn provider_error(
        code: Option<&str>,
        error_type: Option<&str>,
        status: Option<u16>,
        can_retry: bool,
    ) -> LlmError {
        LlmError {
            message: "provider failed".to_owned(),
            provider_code: code.map(ToOwned::to_owned),
            provider_type: error_type.map(ToOwned::to_owned),
            http_status: status,
            can_retry,
            retry_after_ms: None,
            native_error: None,
        }
    }

    fn request() -> LlmRequest {
        LlmRequest {
            model: model(),
            instructions: None,
            messages: Vec::new(),
            tools: Vec::new(),
            provider_options: Map::new(),
            metadata: BTreeMap::new(),
        }
    }

    fn assistant() -> AssistantMessage {
        AssistantMessage {
            id: MessageId::new("assistant-1").expect("message ID"),
            model: model(),
            usage: None,
            duration_ms: 1,
            native_message: json!({"output": []}),
            content: vec![AssistantContent::Response {
                response: TextContent {
                    content: "done".to_owned(),
                    metadata: None,
                },
            }],
            stop_reason: StopReason::Stop,
            timestamp: Timestamp(1),
        }
    }

    fn model() -> ModelRef {
        ModelRef {
            provider: ProviderId::new("openai").expect("provider"),
            id: ModelId::new("gpt-5.6-sol").expect("model"),
            name: None,
        }
    }
}
