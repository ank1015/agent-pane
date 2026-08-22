use std::{fmt, str::FromStr};

use chrono::{DateTime, Utc};
use clap::ValueEnum;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use uuid::Uuid;
use zeroize::{Zeroize, ZeroizeOnDrop, Zeroizing};

use crate::{
    db::{AccountStoreError, Database, NewProviderAccount, ProviderAccount, ResolvedAccount},
    vault::{Vault, VaultError},
};

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize, ValueEnum)]
#[serde(rename_all = "snake_case")]
pub enum ProviderKind {
    Openai,
    Chatgpt,
    Fireworks,
    Anthropic,
    Openrouter,
    Deepseek,
}

impl ProviderKind {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Openai => provider_openai::OPENAI_PROVIDER,
            Self::Chatgpt => provider_chatgpt::CHATGPT_PROVIDER,
            Self::Fireworks => provider_fireworks::FIREWORKS_PROVIDER,
            Self::Anthropic => provider_anthropic::ANTHROPIC_PROVIDER,
            Self::Openrouter => provider_openrouter::OPENROUTER_PROVIDER,
            Self::Deepseek => provider_deepseek::DEEPSEEK_PROVIDER,
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
            provider_anthropic::ANTHROPIC_PROVIDER => Ok(Self::Anthropic),
            provider_openrouter::OPENROUTER_PROVIDER => Ok(Self::Openrouter),
            provider_deepseek::DEEPSEEK_PROVIDER => Ok(Self::Deepseek),
            _ => Err(UnsupportedProvider(value.to_owned())),
        }
    }
}

#[derive(Debug, thiserror::Error)]
#[error("unsupported provider {0:?}")]
pub struct UnsupportedProvider(pub String);

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
    Anthropic {
        api_key: String,
    },
    Openrouter {
        api_key: String,
    },
    Deepseek {
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
            Self::Anthropic { .. } => ProviderKind::Anthropic,
            Self::Openrouter { .. } => ProviderKind::Openrouter,
            Self::Deepseek { .. } => ProviderKind::Deepseek,
        }
    }

    pub fn validate(&self) -> Result<(), AccountServiceError> {
        match self {
            Self::Openai { api_key }
            | Self::Fireworks { api_key }
            | Self::Anthropic { api_key }
            | Self::Openrouter { api_key }
            | Self::Deepseek { api_key } => require_credential(api_key, "API key"),
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
        enabled: bool,
        make_default: bool,
    ) -> Result<ProviderAccount, AccountServiceError> {
        if credentials.provider() != provider {
            return Err(AccountServiceError::CredentialProviderMismatch);
        }
        credentials.validate()?;
        validate_name_and_config(&name, &config)?;

        let account_id = Uuid::now_v7();
        let secret_id = Uuid::now_v7();
        let plaintext = Zeroizing::new(serde_json::to_vec(credentials)?);
        let encrypted = self
            .vault
            .encrypt(account_id, secret_id, provider.as_str(), &plaintext)?;

        self.database
            .create_account(
                NewProviderAccount {
                    id: account_id,
                    provider: provider.as_str().to_owned(),
                    name,
                    secret_id,
                    config,
                    enabled,
                    make_default,
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
            account.secret_id,
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
        let encrypted =
            self.vault
                .encrypt(account.id, account.secret_id, &account.provider, &plaintext)?;
        self.database
            .rotate_credentials(account.id, encrypted)
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
