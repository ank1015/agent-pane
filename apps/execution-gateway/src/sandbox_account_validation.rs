use credential_vault::VaultError;
use serde_json::Value;
use thiserror::Error;
use uuid::Uuid;

use crate::db::DbError;
use crate::sandbox_accounts::SandboxProvider;

pub(crate) fn validate_name(name: &str) -> Result<(), SandboxAccountError> {
    if name.is_empty() || name != name.trim() || name.chars().count() > 120 {
        return Err(SandboxAccountError::InvalidName);
    }
    Ok(())
}

pub(crate) fn validate_config(config: &Value) -> Result<(), SandboxAccountError> {
    if !config.is_object() {
        return Err(SandboxAccountError::ConfigMustBeObject);
    }
    Ok(())
}

pub(crate) fn validate_api_key(api_key: &str) -> Result<(), SandboxAccountError> {
    if api_key.is_empty() || api_key != api_key.trim() || api_key.len() > 65_536 {
        return Err(SandboxAccountError::InvalidApiKey);
    }
    Ok(())
}

pub(crate) fn empty_object() -> Value {
    serde_json::json!({})
}

pub(crate) const fn default_true() -> bool {
    true
}

pub(crate) fn is_unique_violation(error: &DbError) -> bool {
    matches!(error, DbError::Sql(source) if source.as_database_error().is_some_and(|error| error.code().as_deref() == Some("23505")))
}

pub(crate) fn secret_context(
    account_id: Uuid,
    secret_id: Uuid,
    provider: SandboxProvider,
) -> String {
    format!("execution-gateway:v1:sandbox-account:{account_id}:{secret_id}:{provider}")
}

#[derive(Debug, Error)]
pub enum SandboxAccountError {
    #[error("account name must be 1-120 characters without surrounding whitespace")]
    InvalidName,
    #[error("account config must be a JSON object")]
    ConfigMustBeObject,
    #[error("API key must not be blank, have surrounding whitespace, or exceed 65536 bytes")]
    InvalidApiKey,
    #[error("a disabled account cannot be the default")]
    DisabledDefault,
    #[error("an active account with this provider and name already exists")]
    NameConflict,
    #[error("sandbox account was not found")]
    AccountNotFound,
    #[error("sandbox account is disabled")]
    AccountDisabled,
    #[error("sandbox account does not belong to the requested provider")]
    ProviderMismatch,
    #[error("stored sandbox provider is unsupported: {0}")]
    StoredProvider(String),
    #[error("stored credential validation status is unsupported: {0}")]
    StoredValidationStatus(String),
    #[error("decrypted credentials do not match their account provider")]
    CredentialProviderMismatch,
    #[error(transparent)]
    Database(#[from] DbError),
    #[error(transparent)]
    Vault(#[from] VaultError),
    #[error(transparent)]
    Json(#[from] serde_json::Error),
}
