use chrono::{DateTime, Utc};
use llm_contracts::{AssistantMessage, LlmError, LlmRequest};
use serde::{Deserialize, Deserializer, Serialize};
use uuid::Uuid;

/// Persist before submission. Reuse with the same payload to recover a lost response.
/// Keys are shared by all runtime callers of a gateway database.
#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
#[serde(transparent)]
pub struct IdempotencyKey(String);

impl IdempotencyKey {
    pub fn new(value: impl Into<String>) -> Result<Self, InvalidIdempotencyKey> {
        let value = value.into();
        if value.is_empty() || value.len() > 256 || !value.bytes().all(|b| b.is_ascii_graphic()) {
            return Err(InvalidIdempotencyKey);
        }
        Ok(Self(value))
    }

    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl<'de> Deserialize<'de> for IdempotencyKey {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        Self::new(String::deserialize(deserializer)?).map_err(serde::de::Error::custom)
    }
}

#[derive(Debug, thiserror::Error)]
#[error("idempotency key must contain 1–256 visible ASCII characters")]
pub struct InvalidIdempotencyKey;

/// Gateway routing options and the provider-neutral completion input.
#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct CompletionRequest {
    #[serde(default)]
    pub account_id: Option<Uuid>,
    pub request: LlmRequest,
}

impl CompletionRequest {
    /// Use the provider's default gateway account.
    #[must_use]
    pub fn new(request: LlmRequest) -> Self {
        Self {
            account_id: None,
            request,
        }
    }

    #[must_use]
    pub fn with_account(mut self, account_id: Uuid) -> Self {
        self.account_id = Some(account_id);
        self
    }
}

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
pub struct CompletionResponse {
    pub request_id: Uuid,
    pub account_id: Uuid,
    pub message: AssistantMessage,
}

/// Snapshot of a gateway run. A failed run is a successfully retrieved resource.
#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
pub struct Run {
    pub run_id: Uuid,
    #[serde(flatten)]
    pub state: RunState,
    pub created_at: DateTime<Utc>,
    pub completed_at: Option<DateTime<Utc>>,
    pub expires_at: Option<DateTime<Utc>>,
}

/// Terminal results are typed according to their status, never arbitrary JSON.
#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
#[serde(tag = "status", content = "result", rename_all = "snake_case")]
pub enum RunState {
    Running,
    Succeeded(Box<CompletionResponse>),
    Failed(GatewayFailure),
    Expired,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
pub struct GatewayFailure {
    pub request_id: Uuid,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub account_id: Option<Uuid>,
    pub error: GatewayError,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
pub struct GatewayError {
    pub kind: GatewayErrorKind,
    pub message: String,
    pub can_retry: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub retry_after_ms: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub provider_error: Option<Box<LlmError>>,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum GatewayErrorKind {
    Unauthorized,
    AuthenticationRateLimited,
    InvalidRequest,
    Conflict,
    UnsupportedCapability,
    UnknownModel,
    AccountNotFound,
    RequestNotFound,
    AccountDisabled,
    AccountProviderMismatch,
    DefaultAccountNotFound,
    AuthenticationRequired,
    Overloaded,
    Provider,
    ProviderTimeout,
    Internal,
    /// A newer gateway error category; message and retry metadata remain available.
    #[serde(other)]
    Unknown,
}
