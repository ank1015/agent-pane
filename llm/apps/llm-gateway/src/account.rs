use std::{fmt, str::FromStr};

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use uuid::Uuid;
use zeroize::{Zeroize, ZeroizeOnDrop, Zeroizing};

use crate::{
    db::{AccountStoreError, Database, NewProviderAccount, ProviderAccount, ResolvedAccount},
    vault::{Vault, VaultError},
};

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ProviderKind {
    Openai,
    Chatgpt,
    Fireworks,
}

impl ProviderKind {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Openai => provider_openai::OPENAI_PROVIDER,
            Self::Chatgpt => provider_chatgpt::CHATGPT_PROVIDER,
            Self::Fireworks => provider_fireworks::FIREWORKS_PROVIDER,
        }
    }
}

impl fmt::Display for ProviderKind {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.as_str())
    }
}

impl FromStr for ProviderKind {
    type Err = UnsupportedProvider;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        match value {
            provider_openai::OPENAI_PROVIDER => Ok(Self::Openai),
            provider_chatgpt::CHATGPT_PROVIDER => Ok(Self::Chatgpt),
            provider_fireworks::FIREWORKS_PROVIDER => Ok(Self::Fireworks),
            _ => Err(UnsupportedProvider(value.to_owned())),
        }
    }
}

#[derive(Debug, thiserror::Error)]
#[error("unsupported provider {0:?}")]
pub struct UnsupportedProvider(pub String);

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum AccountStatus {
    Active,
    Disabled,
    ReauthRequired,
}

impl AccountStatus {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Active => "active",
            Self::Disabled => "disabled",
            Self::ReauthRequired => "reauth_required",
        }
    }
}

impl fmt::Display for AccountStatus {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.as_str())
    }
}

impl FromStr for AccountStatus {
    type Err = UnsupportedAccountStatus;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        match value {
            "active" => Ok(Self::Active),
            "disabled" => Ok(Self::Disabled),
            "reauth_required" => Ok(Self::ReauthRequired),
            _ => Err(UnsupportedAccountStatus(value.to_owned())),
        }
    }
}

#[derive(Debug, thiserror::Error)]
#[error("unsupported account status {0:?}")]
pub struct UnsupportedAccountStatus(pub String);

#[derive(Deserialize, Serialize, Zeroize, ZeroizeOnDrop)]
#[serde(tag = "provider", rename_all = "snake_case")]
pub enum ProviderCredentials {
    Openai {
        api_key: String,
    },
    Chatgpt {
        access_token: String,
        account_id: String,
        #[serde(default)]
        id_token: String,
        #[serde(default)]
        refresh_token: String,
        #[serde(default)]
        #[zeroize(skip)]
        access_token_expires_at: Option<DateTime<Utc>>,
        #[serde(default)]
        #[zeroize(skip)]
        refreshed_at: Option<DateTime<Utc>>,
    },
    Fireworks {
        api_key: String,
    },
}

impl ProviderCredentials {
    #[must_use]
    pub const fn provider(&self) -> ProviderKind {
        match self {
            Self::Openai { .. } => ProviderKind::Openai,
            Self::Chatgpt { .. } => ProviderKind::Chatgpt,
            Self::Fireworks { .. } => ProviderKind::Fireworks,
        }
    }

    #[must_use]
    pub fn expires_at(&self) -> Option<DateTime<Utc>> {
        match self {
            Self::Chatgpt {
                access_token_expires_at,
                ..
            } => *access_token_expires_at,
            _ => None,
        }
    }

    #[must_use]
    pub fn refreshed_at(&self) -> Option<DateTime<Utc>> {
        match self {
            Self::Chatgpt { refreshed_at, .. } => *refreshed_at,
            _ => None,
        }
    }

    pub fn validate(&self) -> Result<(), AccountServiceError> {
        match self {
            Self::Openai { api_key } | Self::Fireworks { api_key } => {
                require_credential(api_key, "API key")
            }
            Self::Chatgpt {
                access_token,
                account_id,
                ..
            } => {
                require_credential(access_token, "ChatGPT access token")?;
                require_credential(account_id, "ChatGPT account ID")
            }
        }
    }
}

#[derive(Clone)]
pub struct AccountService {
    database: Database,
    vault: Vault,
}

impl AccountService {
    #[must_use]
    pub const fn new(database: Database, vault: Vault) -> Self {
        Self { database, vault }
    }

    pub async fn create(
        &self,
        provider: ProviderKind,
        name: String,
        credentials: &ProviderCredentials,
        config: Value,
        status: AccountStatus,
        make_default: bool,
    ) -> Result<ProviderAccount, AccountServiceError> {
        if credentials.provider() != provider {
            return Err(AccountServiceError::CredentialProviderMismatch);
        }
        credentials.validate()?;
        validate_name_and_config(&name, &config)?;
        let account_id = Uuid::now_v7();
        let plaintext = Zeroizing::new(serde_json::to_vec(credentials)?);
        let encrypted = self
            .vault
            .encrypt(account_id, provider.as_str(), &plaintext)?;
        self.database
            .create_account(
                NewProviderAccount {
                    id: account_id,
                    provider: provider.as_str().to_owned(),
                    name,
                    config,
                    status: status.as_str().to_owned(),
                    make_default,
                    credential_expires_at: credentials.expires_at(),
                    credential_refreshed_at: credentials.refreshed_at(),
                },
                encrypted,
            )
            .await
            .map_err(Into::into)
    }

    pub fn decrypt(
        &self,
        account: &ResolvedAccount,
    ) -> Result<ProviderCredentials, AccountServiceError> {
        let plaintext = self.vault.decrypt(
            account.id,
            &account.provider,
            &account.encrypted_payload,
            &account.nonce,
            account.encryption_key_version,
        )?;
        let credentials: ProviderCredentials = serde_json::from_slice(&plaintext)?;
        let provider = account.provider.parse::<ProviderKind>()?;
        if credentials.provider() != provider {
            return Err(AccountServiceError::CredentialProviderMismatch);
        }
        credentials.validate()?;
        Ok(credentials)
    }

    pub async fn rotate_credentials(
        &self,
        account: &ProviderAccount,
        credentials: &ProviderCredentials,
    ) -> Result<i64, AccountServiceError> {
        let provider = account.provider.parse::<ProviderKind>()?;
        if credentials.provider() != provider {
            return Err(AccountServiceError::CredentialProviderMismatch);
        }
        credentials.validate()?;
        let plaintext = Zeroizing::new(serde_json::to_vec(credentials)?);
        let encrypted = self
            .vault
            .encrypt(account.id, &account.provider, &plaintext)?;
        self.database
            .rotate_credentials(
                account.id,
                encrypted,
                credentials.expires_at(),
                credentials.refreshed_at(),
            )
            .await
            .map_err(Into::into)
    }
}

fn validate_name_and_config(name: &str, config: &Value) -> Result<(), AccountServiceError> {
    if name.trim().is_empty() || name != name.trim() {
        return Err(AccountServiceError::InvalidName);
    }
    if !config.is_object() {
        return Err(AccountServiceError::ConfigMustBeObject);
    }
    Ok(())
}

fn require_credential(value: &str, name: &'static str) -> Result<(), AccountServiceError> {
    if value.trim().is_empty() {
        return Err(AccountServiceError::InvalidCredential(name));
    }
    Ok(())
}

#[derive(Debug, thiserror::Error)]
pub enum AccountServiceError {
    #[error("account name must not be blank or contain surrounding whitespace")]
    InvalidName,
    #[error("account config must be a JSON object")]
    ConfigMustBeObject,
    #[error("credential type does not match the account provider")]
    CredentialProviderMismatch,
    #[error("{0} must not be empty")]
    InvalidCredential(&'static str),
    #[error(transparent)]
    UnsupportedProvider(#[from] UnsupportedProvider),
    #[error(transparent)]
    Serialization(#[from] serde_json::Error),
    #[error(transparent)]
    Vault(#[from] VaultError),
    #[error(transparent)]
    Store(#[from] AccountStoreError),
}

#[cfg(test)]
mod tests {
    use std::str::FromStr;

    use super::{AccountStatus, ProviderKind};

    #[test]
    fn provider_and_status_values_are_strict() {
        assert_eq!(
            ProviderKind::from_str("openai").unwrap(),
            ProviderKind::Openai
        );
        assert!(ProviderKind::from_str("anthropic").is_err());
        assert_eq!(
            AccountStatus::from_str("reauth_required").unwrap(),
            AccountStatus::ReauthRequired
        );
        assert!(AccountStatus::from_str("deleted").is_err());
    }
}
