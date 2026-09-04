use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use uuid::Uuid;
use zeroize::{Zeroize, ZeroizeOnDrop};

use super::error::ProviderError;

// Never derive Debug for credential-bearing inputs.
#[derive(Deserialize, Zeroize, ZeroizeOnDrop)]
#[serde(deny_unknown_fields)]
pub struct CreateProviderInput {
    #[zeroize(skip)]
    pub provider: ProviderKind,
    pub name: String,
    pub api_key: String,
}

pub(super) fn validate_name(name: &str) -> Result<(), ProviderError> {
    if name.trim().is_empty()
        || name.trim() != name
        || name.chars().count() > 200
        || name.chars().any(char::is_control)
    {
        return Err(ProviderError::InvalidRequest(
            "Account name must contain 1–200 characters without surrounding whitespace or control characters.",
        ));
    }
    Ok(())
}

impl CreateProviderInput {
    pub fn validate(&self) -> Result<(), ProviderError> {
        validate_name(&self.name)?;
        if self.provider == ProviderKind::Chatgpt {
            return Err(ProviderError::InvalidRequest(
                "Use Sign In with ChatGPT to add a ChatGPT account.",
            ));
        }
        if self.api_key.is_empty()
            || self.api_key.len() > 4096
            || self.api_key.trim() != self.api_key
            || self.api_key.chars().any(char::is_control)
        {
            return Err(ProviderError::InvalidRequest(
                "API key must contain 1–4096 bytes without surrounding whitespace or control characters.",
            ));
        }
        Ok(())
    }
}

#[derive(Deserialize)]
pub(super) struct GatewayAccountResponse {
    pub account: GatewayAccount,
}

#[derive(Serialize)]
pub(super) struct GatewayCreateAccount<'a, T: Serialize> {
    pub provider: ProviderKind,
    pub name: &'a str,
    pub credentials: &'a T,
}

#[derive(Serialize)]
pub(super) struct ApiKeyCredentials<'a> {
    pub api_key: &'a str,
}

#[derive(Serialize, Zeroize, ZeroizeOnDrop)]
pub(super) struct ChatGptCredentials {
    pub id_token: String,
    pub access_token: String,
    pub refresh_token: String,
    pub account_id: String,
    #[zeroize(skip)]
    pub access_token_expires_at: DateTime<Utc>,
}

// Mirrors llm/apps/llm-gateway's account-list wire contract without depending
// on its database, vault, or provider execution implementation.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Ord, PartialOrd, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ProviderKind {
    Chatgpt,
    Fireworks,
    Openai,
}

#[derive(Clone, Copy, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(super) enum GatewayAccountStatus {
    Active,
    Disabled,
    ReauthRequired,
}

#[derive(Deserialize)]
pub(super) struct GatewayAccountsResponse {
    pub accounts: Vec<GatewayAccount>,
}

// Allow upstream additions, but only deserialize the fields the dashboard
// needs. Config and credential metadata never enter the public response.
#[derive(Deserialize)]
pub(super) struct GatewayAccount {
    pub id: Uuid,
    pub name: String,
    pub provider: ProviderKind,
    pub config: serde_json::Value,
    pub status: GatewayAccountStatus,
    pub runtime_revision: i64,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
    pub is_default: bool,
    pub credential: ProviderCredentialMetadata,
}

/// Safe administrative detail for one account. Credential values are never
/// represented by this type; only the gateway's rotation/expiry metadata is.
#[derive(Debug, Deserialize, Serialize)]
pub struct ProviderCredentialMetadata {
    pub version: i64,
    pub encryption_key_version: i32,
    #[serde(default)]
    pub expires_at: Option<DateTime<Utc>>,
    #[serde(default)]
    pub refreshed_at: Option<DateTime<Utc>>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Serialize)]
pub struct ProviderDetail {
    pub id: Uuid,
    pub name: String,
    pub provider: ProviderKind,
    pub config: serde_json::Value,
    pub status: ProviderAccountStatus,
    pub runtime_revision: i64,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
    pub is_default: bool,
    pub credential: ProviderCredentialMetadata,
}

#[derive(Debug, Serialize)]
pub struct ProviderDetailResponse {
    pub provider: ProviderDetail,
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

#[derive(Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ProviderAccountStatus {
    Enabled,
    Disabled,
    ReauthRequired,
}

impl From<GatewayAccount> for ProviderAccountSummary {
    fn from(account: GatewayAccount) -> Self {
        Self {
            id: account.id,
            name: account.name,
            provider: account.provider,
            status: account.status.into(),
            created_at: account.created_at,
            is_default: account.is_default,
        }
    }
}

impl From<GatewayAccount> for ProviderDetail {
    fn from(account: GatewayAccount) -> Self {
        Self {
            id: account.id,
            name: account.name,
            provider: account.provider,
            config: account.config,
            status: account.status.into(),
            runtime_revision: account.runtime_revision,
            created_at: account.created_at,
            updated_at: account.updated_at,
            is_default: account.is_default,
            credential: account.credential,
        }
    }
}

impl From<GatewayAccountStatus> for ProviderAccountStatus {
    fn from(status: GatewayAccountStatus) -> Self {
        match status {
            GatewayAccountStatus::Active => Self::Enabled,
            GatewayAccountStatus::Disabled => Self::Disabled,
            GatewayAccountStatus::ReauthRequired => Self::ReauthRequired,
        }
    }
}
