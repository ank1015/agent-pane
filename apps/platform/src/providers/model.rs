use std::fmt;

use chrono::{DateTime, Utc};
use llm_contracts::Usage;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use uuid::Uuid;
use zeroize::{Zeroize, ZeroizeOnDrop};

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ProviderKind {
    Openai,
    Chatgpt,
    Fireworks,
    Anthropic,
    Openrouter,
    Deepseek,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ApiKeyProviderKind {
    Openai,
    Fireworks,
    Anthropic,
    Openrouter,
    Deepseek,
}

impl From<ApiKeyProviderKind> for ProviderKind {
    fn from(provider: ApiKeyProviderKind) -> Self {
        match provider {
            ApiKeyProviderKind::Openai => Self::Openai,
            ApiKeyProviderKind::Fireworks => Self::Fireworks,
            ApiKeyProviderKind::Anthropic => Self::Anthropic,
            ApiKeyProviderKind::Openrouter => Self::Openrouter,
            ApiKeyProviderKind::Deepseek => Self::Deepseek,
        }
    }
}

impl ProviderKind {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Openai => "openai",
            Self::Chatgpt => "chatgpt",
            Self::Fireworks => "fireworks",
            Self::Anthropic => "anthropic",
            Self::Openrouter => "openrouter",
            Self::Deepseek => "deepseek",
        }
    }
}

impl fmt::Display for ProviderKind {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.as_str())
    }
}

#[derive(Deserialize, Zeroize, ZeroizeOnDrop)]
#[serde(deny_unknown_fields)]
pub struct CreateProviderRequest {
    #[zeroize(skip)]
    pub provider: ApiKeyProviderKind,
    pub name: String,
    pub api_key: String,
}

#[derive(Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct UpdateProviderRequest {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub config: Option<Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub enabled: Option<bool>,
}

#[derive(Deserialize, Serialize, Zeroize, ZeroizeOnDrop)]
#[serde(untagged)]
pub enum RotateCredentialsRequest {
    Chatgpt(ChatGptCredentials),
    ApiKey(ApiKeyCredentials),
}

#[derive(Deserialize, Serialize, Zeroize, ZeroizeOnDrop)]
#[serde(deny_unknown_fields)]
pub struct ApiKeyCredentials {
    pub api_key: String,
}

#[derive(Deserialize, Serialize, Zeroize, ZeroizeOnDrop)]
#[serde(deny_unknown_fields)]
pub struct ChatGptCredentials {
    pub id_token: String,
    pub access_token: String,
    pub refresh_token: String,
    pub account_id: String,
    #[zeroize(skip)]
    pub access_token_expires_at: DateTime<Utc>,
}

#[derive(Debug, Deserialize, Serialize)]
pub struct Provider {
    pub id: Uuid,
    pub provider: ProviderKind,
    pub name: String,
    pub config: Value,
    pub enabled: bool,
    pub is_default: bool,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
    pub credential: CredentialMetadata,
}

#[derive(Debug, Deserialize, Serialize)]
pub struct CredentialMetadata {
    pub version: i64,
    pub encryption_key_version: i32,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Eq, PartialEq, Serialize)]
pub struct ProviderAccountSummary {
    pub id: Uuid,
    pub name: String,
    pub provider: ProviderKind,
    pub status: ProviderAccountStatus,
    pub created_at: DateTime<Utc>,
    pub is_default: bool,
}

impl From<Provider> for ProviderAccountSummary {
    fn from(account: Provider) -> Self {
        Self {
            id: account.id,
            name: account.name,
            provider: account.provider,
            status: if account.enabled {
                ProviderAccountStatus::Enabled
            } else {
                ProviderAccountStatus::Disabled
            },
            created_at: account.created_at,
            is_default: account.is_default,
        }
    }
}

#[derive(Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ProviderAccountStatus {
    Enabled,
    Disabled,
}

#[derive(Deserialize)]
pub(crate) struct GatewayProviderResponse {
    pub account: Provider,
}

#[derive(Deserialize)]
pub(crate) struct GatewayProvidersResponse {
    pub accounts: Vec<Provider>,
}

#[derive(Serialize)]
pub(crate) struct ProviderResponse {
    pub provider: Provider,
}

#[derive(Debug, Deserialize, Serialize)]
pub struct ProviderUsageSummary {
    pub account_id: Uuid,
    pub request_count: i64,
    pub costs: ProviderCostTotals,
    pub tokens: ProviderTokenTotals,
}

#[derive(Debug, Deserialize, Serialize)]
pub struct ProviderCostTotals {
    pub total: f64,
    pub input: f64,
    pub output: f64,
    pub cache_read: f64,
    pub cache_write: f64,
}

#[derive(Debug, Deserialize, Serialize)]
pub struct ProviderTokenTotals {
    pub input: i64,
    pub output: i64,
    pub cache_read: i64,
    pub cache_write: i64,
}

#[derive(Debug, Deserialize, Serialize)]
pub struct ProviderRequestPage {
    pub items: Vec<ProviderRequest>,
    pub next_cursor: Option<String>,
}

#[derive(Debug, Deserialize, Serialize)]
pub struct ProviderRequest {
    pub request_id: Uuid,
    pub requested_provider: String,
    pub requested_model: String,
    pub response_provider: String,
    pub response_model: String,
    pub assistant_message_id: String,
    pub usage: Option<Usage>,
    pub duration_ms: i64,
    pub completed_at: DateTime<Utc>,
}

#[derive(Debug, Default, Deserialize, Serialize)]
#[serde(default, deny_unknown_fields)]
pub struct ProviderRequestsQuery {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cursor: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub limit: Option<u32>,
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::{ApiKeyProviderKind, CreateProviderRequest};

    #[test]
    fn create_request_accepts_api_key_providers() {
        let request: CreateProviderRequest = serde_json::from_value(json!({
            "provider": "openai",
            "name": "personal",
            "api_key": "secret"
        }))
        .unwrap();

        assert_eq!(request.provider, ApiKeyProviderKind::Openai);
        assert_eq!(request.name, "personal");
        assert_eq!(request.api_key, "secret");
    }

    #[test]
    fn create_request_rejects_chatgpt_until_its_flow_is_supported() {
        let result = serde_json::from_value::<CreateProviderRequest>(json!({
            "provider": "chatgpt",
            "name": "personal",
            "api_key": "secret"
        }));

        assert!(result.is_err());
    }
}
