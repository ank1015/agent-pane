use std::{fmt, str::FromStr};

use chrono::{DateTime, Utc};
use credential_vault::{CredentialVault, EncryptedSecret, VaultError};
use execution_contracts::TimestampMs;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use sqlx::Row;
use thiserror::Error;
use uuid::Uuid;
use zeroize::{Zeroize, ZeroizeOnDrop, Zeroizing};

use crate::db::{Database, DbError};

#[derive(Clone, Copy, Debug, Deserialize, Eq, Ord, PartialEq, PartialOrd, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum SandboxProvider {
    E2b,
    Daytona,
    Blaxel,
    Tensorlake,
}

impl SandboxProvider {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::E2b => "e2b",
            Self::Daytona => "daytona",
            Self::Blaxel => "blaxel",
            Self::Tensorlake => "tensorlake",
        }
    }
}

impl fmt::Display for SandboxProvider {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.as_str())
    }
}

impl FromStr for SandboxProvider {
    type Err = SandboxAccountError;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        match value {
            "e2b" => Ok(Self::E2b),
            "daytona" => Ok(Self::Daytona),
            "blaxel" => Ok(Self::Blaxel),
            "tensorlake" => Ok(Self::Tensorlake),
            _ => Err(SandboxAccountError::StoredProvider(value.to_owned())),
        }
    }
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum CredentialValidationStatus {
    Unchecked,
    Valid,
    Invalid,
}

impl FromStr for CredentialValidationStatus {
    type Err = SandboxAccountError;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        match value {
            "unchecked" => Ok(Self::Unchecked),
            "valid" => Ok(Self::Valid),
            "invalid" => Ok(Self::Invalid),
            _ => Err(SandboxAccountError::StoredValidationStatus(
                value.to_owned(),
            )),
        }
    }
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct SandboxAccount {
    pub id: Uuid,
    pub provider: SandboxProvider,
    pub name: String,
    pub config: Value,
    pub enabled: bool,
    pub is_default: bool,
    pub validation_status: CredentialValidationStatus,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_validated_at: Option<TimestampMs>,
    pub credential_version: i64,
    pub credentials_updated_at: TimestampMs,
    pub created_at: TimestampMs,
    pub updated_at: TimestampMs,
}

#[derive(Deserialize)]
pub struct CreateSandboxAccountRequest {
    pub provider: SandboxProvider,
    pub name: String,
    pub api_key: String,
    #[serde(default = "empty_object")]
    pub config: Value,
    #[serde(default = "default_true")]
    pub enabled: bool,
    #[serde(default)]
    pub make_default: bool,
}

#[derive(Deserialize)]
pub struct RotateSandboxCredentialsRequest {
    pub api_key: String,
}

#[derive(Deserialize)]
pub struct UpdateSandboxAccountNameRequest {
    pub name: String,
}

#[derive(Deserialize, Serialize, Zeroize, ZeroizeOnDrop)]
#[serde(tag = "provider", rename_all = "snake_case")]
pub enum SandboxCredentials {
    E2b { api_key: String },
    Daytona { api_key: String },
    Blaxel { api_key: String },
    Tensorlake { api_key: String },
}

impl SandboxCredentials {
    fn new(provider: SandboxProvider, api_key: String) -> Self {
        match provider {
            SandboxProvider::E2b => Self::E2b { api_key },
            SandboxProvider::Daytona => Self::Daytona { api_key },
            SandboxProvider::Blaxel => Self::Blaxel { api_key },
            SandboxProvider::Tensorlake => Self::Tensorlake { api_key },
        }
    }

    pub const fn provider(&self) -> SandboxProvider {
        match self {
            Self::E2b { .. } => SandboxProvider::E2b,
            Self::Daytona { .. } => SandboxProvider::Daytona,
            Self::Blaxel { .. } => SandboxProvider::Blaxel,
            Self::Tensorlake { .. } => SandboxProvider::Tensorlake,
        }
    }

    pub fn api_key(&self) -> &str {
        match self {
            Self::E2b { api_key }
            | Self::Daytona { api_key }
            | Self::Blaxel { api_key }
            | Self::Tensorlake { api_key } => api_key,
        }
    }
}

impl fmt::Debug for SandboxCredentials {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("SandboxCredentials")
            .field("provider", &self.provider())
            .field("api_key", &"[REDACTED]")
            .finish()
    }
}

#[derive(Clone)]
pub struct SandboxAccountService {
    database: Database,
    vault: CredentialVault,
}

impl SandboxAccountService {
    pub const fn new(database: Database, vault: CredentialVault) -> Self {
        Self { database, vault }
    }

    pub async fn create(
        &self,
        request: CreateSandboxAccountRequest,
    ) -> Result<SandboxAccount, SandboxAccountError> {
        validate_name(&request.name)?;
        validate_config(&request.config)?;
        validate_api_key(&request.api_key)?;
        if request.make_default && !request.enabled {
            return Err(SandboxAccountError::DisabledDefault);
        }
        let account_id = Uuid::now_v7();
        let secret_id = Uuid::now_v7();
        let credentials = SandboxCredentials::new(request.provider, request.api_key);
        let plaintext = Zeroizing::new(serde_json::to_vec(&credentials)?);
        let encrypted = self.vault.encrypt(
            &secret_context(account_id, secret_id, request.provider),
            &plaintext,
        )?;
        match self
            .database
            .create_sandbox_account(
                NewSandboxAccount {
                    id: account_id,
                    provider: request.provider,
                    name: request.name,
                    secret_id,
                    config: request.config,
                    enabled: request.enabled,
                    make_default: request.make_default,
                },
                encrypted,
            )
            .await
        {
            Err(error) if is_unique_violation(&error) => Err(SandboxAccountError::NameConflict),
            result => result.map_err(Into::into),
        }
    }

    pub async fn list(&self) -> Result<Vec<SandboxAccount>, SandboxAccountError> {
        self.database.sandbox_accounts().await.map_err(Into::into)
    }

    pub async fn find(&self, id: Uuid) -> Result<Option<SandboxAccount>, SandboxAccountError> {
        self.database.sandbox_account(id).await.map_err(Into::into)
    }

    pub async fn credentials(
        &self,
        id: Uuid,
    ) -> Result<Option<SandboxCredentials>, SandboxAccountError> {
        let Some(secret) = self.database.resolved_sandbox_account(id).await? else {
            return Ok(None);
        };
        let plaintext = self.vault.decrypt(
            &secret_context(secret.account_id, secret.secret_id, secret.provider),
            &secret.encrypted_payload,
            &secret.nonce,
            secret.encryption_key_version,
        )?;
        let credentials: SandboxCredentials = serde_json::from_slice(&plaintext)?;
        if credentials.provider() != secret.provider {
            return Err(SandboxAccountError::CredentialProviderMismatch);
        }
        validate_api_key(credentials.api_key())?;
        Ok(Some(credentials))
    }

    pub async fn rotate_credentials(
        &self,
        id: Uuid,
        request: RotateSandboxCredentialsRequest,
    ) -> Result<Option<SandboxAccount>, SandboxAccountError> {
        validate_api_key(&request.api_key)?;
        let Some(secret) = self.database.resolved_sandbox_account(id).await? else {
            return Ok(None);
        };
        let credentials = SandboxCredentials::new(secret.provider, request.api_key);
        let plaintext = Zeroizing::new(serde_json::to_vec(&credentials)?);
        let encrypted = self.vault.encrypt(
            &secret_context(secret.account_id, secret.secret_id, secret.provider),
            &plaintext,
        )?;
        self.database
            .rotate_sandbox_credentials(id, secret.secret_id, encrypted)
            .await?;
        self.find(id).await
    }

    pub async fn update_name(
        &self,
        id: Uuid,
        request: UpdateSandboxAccountNameRequest,
    ) -> Result<Option<SandboxAccount>, SandboxAccountError> {
        validate_name(&request.name)?;
        match self
            .database
            .update_sandbox_account_name(id, &request.name)
            .await
        {
            Err(error) if is_unique_violation(&error) => Err(SandboxAccountError::NameConflict),
            result => result.map_err(Into::into),
        }
    }

    pub async fn delete(&self, id: Uuid) -> Result<bool, SandboxAccountError> {
        self.database
            .delete_sandbox_account(id)
            .await
            .map_err(Into::into)
    }
}

struct NewSandboxAccount {
    id: Uuid,
    provider: SandboxProvider,
    name: String,
    secret_id: Uuid,
    config: Value,
    enabled: bool,
    make_default: bool,
}

struct ResolvedSandboxAccount {
    account_id: Uuid,
    provider: SandboxProvider,
    secret_id: Uuid,
    encrypted_payload: Vec<u8>,
    nonce: Vec<u8>,
    encryption_key_version: i32,
}

impl Database {
    async fn create_sandbox_account(
        &self,
        account: NewSandboxAccount,
        secret: EncryptedSecret,
    ) -> Result<SandboxAccount, DbError> {
        let mut transaction = self.pool().begin().await?;
        sqlx::query("select pg_advisory_xact_lock(hashtext($1))")
            .bind(account.provider.as_str())
            .execute(&mut *transaction)
            .await?;
        let has_default: bool = sqlx::query_scalar(
            "select exists(select 1 from sandbox_accounts where provider = $1 and is_default and deleted_at is null)",
        )
        .bind(account.provider.as_str())
        .fetch_one(&mut *transaction)
        .await?;
        let is_default = account.enabled && (account.make_default || !has_default);
        if is_default {
            sqlx::query("update sandbox_accounts set is_default = false where provider = $1 and is_default and deleted_at is null")
                .bind(account.provider.as_str())
                .execute(&mut *transaction)
                .await?;
        }
        sqlx::query(
            "insert into execution_vault_secrets
             (id, encrypted_payload, nonce, encryption_key_version)
             values ($1, $2, $3, $4)",
        )
        .bind(account.secret_id)
        .bind(secret.encrypted_payload)
        .bind(secret.nonce)
        .bind(secret.encryption_key_version)
        .execute(&mut *transaction)
        .await?;
        sqlx::query(
            "insert into sandbox_accounts
             (id, provider, name, secret_id, config, enabled, is_default)
             values ($1, $2, $3, $4, $5, $6, $7)",
        )
        .bind(account.id)
        .bind(account.provider.as_str())
        .bind(account.name)
        .bind(account.secret_id)
        .bind(account.config)
        .bind(account.enabled)
        .bind(is_default)
        .execute(&mut *transaction)
        .await?;
        transaction.commit().await?;
        self.sandbox_account(account.id)
            .await?
            .ok_or_else(|| DbError::Contract("created sandbox account disappeared".to_owned()))
    }

    async fn sandbox_accounts(&self) -> Result<Vec<SandboxAccount>, DbError> {
        sqlx::query(&format!(
            "{SANDBOX_ACCOUNT_SELECT} order by a.provider, lower(a.name), a.created_at"
        ))
        .fetch_all(self.pool())
        .await?
        .into_iter()
        .map(sandbox_account_from_row)
        .collect()
    }

    async fn sandbox_account(&self, id: Uuid) -> Result<Option<SandboxAccount>, DbError> {
        sqlx::query(&format!("{SANDBOX_ACCOUNT_SELECT} and a.id = $1"))
            .bind(id)
            .fetch_optional(self.pool())
            .await?
            .map(sandbox_account_from_row)
            .transpose()
    }

    async fn resolved_sandbox_account(
        &self,
        id: Uuid,
    ) -> Result<Option<ResolvedSandboxAccount>, DbError> {
        let row = sqlx::query(
            "select a.id, a.provider, a.secret_id, s.encrypted_payload, s.nonce,
                    s.encryption_key_version
             from sandbox_accounts a
             join execution_vault_secrets s on s.id = a.secret_id
             where a.id = $1 and a.deleted_at is null",
        )
        .bind(id)
        .fetch_optional(self.pool())
        .await?;
        row.map(|row| {
            Ok(ResolvedSandboxAccount {
                account_id: row.try_get("id")?,
                provider: row
                    .try_get::<String, _>("provider")?
                    .parse()
                    .map_err(|error: SandboxAccountError| DbError::Contract(error.to_string()))?,
                secret_id: row.try_get("secret_id")?,
                encrypted_payload: row.try_get("encrypted_payload")?,
                nonce: row.try_get("nonce")?,
                encryption_key_version: row.try_get("encryption_key_version")?,
            })
        })
        .transpose()
    }

    async fn rotate_sandbox_credentials(
        &self,
        account_id: Uuid,
        secret_id: Uuid,
        secret: EncryptedSecret,
    ) -> Result<(), DbError> {
        let result = sqlx::query(
            "update execution_vault_secrets s
             set encrypted_payload = $3, nonce = $4, encryption_key_version = $5,
                 credential_version = credential_version + 1
             from sandbox_accounts a
             where a.id = $1 and a.secret_id = $2 and a.deleted_at is null and s.id = a.secret_id",
        )
        .bind(account_id)
        .bind(secret_id)
        .bind(secret.encrypted_payload)
        .bind(secret.nonce)
        .bind(secret.encryption_key_version)
        .execute(self.pool())
        .await?;
        if result.rows_affected() != 1 {
            return Err(DbError::Contract(
                "sandbox account disappeared during credential rotation".to_owned(),
            ));
        }
        Ok(())
    }

    async fn update_sandbox_account_name(
        &self,
        id: Uuid,
        name: &str,
    ) -> Result<Option<SandboxAccount>, DbError> {
        let result = sqlx::query(
            "update sandbox_accounts set name = $2, updated_at = now()
             where id = $1 and deleted_at is null",
        )
        .bind(id)
        .bind(name)
        .execute(self.pool())
        .await?;
        if result.rows_affected() == 0 {
            return Ok(None);
        }
        self.sandbox_account(id).await
    }

    async fn delete_sandbox_account(&self, id: Uuid) -> Result<bool, DbError> {
        let mut transaction = self.pool().begin().await?;
        let row = sqlx::query(
            "select provider, secret_id, is_default from sandbox_accounts
             where id = $1 and deleted_at is null for update",
        )
        .bind(id)
        .fetch_optional(&mut *transaction)
        .await?;
        let Some(row) = row else {
            return Ok(false);
        };
        let has_active_machines: bool = sqlx::query_scalar(
            "select exists(select 1 from sandbox_machines where sandbox_account_id = $1 and state not in ('terminated', 'failed'))",
        )
        .bind(id)
        .fetch_one(&mut *transaction)
        .await?;
        if has_active_machines {
            return Err(DbError::Contract(
                "sandbox account owns active machines".to_owned(),
            ));
        }
        let provider: String = row.try_get("provider")?;
        let secret_id: Uuid = row.try_get("secret_id")?;
        let was_default: bool = row.try_get("is_default")?;
        sqlx::query(
            "update sandbox_accounts set enabled = false, is_default = false,
             secret_id = null, deleted_at = now() where id = $1",
        )
        .bind(id)
        .execute(&mut *transaction)
        .await?;
        sqlx::query("delete from execution_vault_secrets where id = $1")
            .bind(secret_id)
            .execute(&mut *transaction)
            .await?;
        if was_default {
            sqlx::query(
                "update sandbox_accounts set is_default = true
                 where id = (select id from sandbox_accounts where provider = $1 and enabled
                    and deleted_at is null order by created_at, id limit 1)",
            )
            .bind(provider)
            .execute(&mut *transaction)
            .await?;
        }
        transaction.commit().await?;
        Ok(true)
    }
}

const SANDBOX_ACCOUNT_SELECT: &str =
    "select a.id, a.provider, a.name, a.config, a.enabled, a.is_default,
            a.validation_status, a.last_validated_at, a.created_at, a.updated_at,
            s.credential_version, s.updated_at as credentials_updated_at
     from sandbox_accounts a
     join execution_vault_secrets s on s.id = a.secret_id
     where a.deleted_at is null";

fn sandbox_account_from_row(row: sqlx::postgres::PgRow) -> Result<SandboxAccount, DbError> {
    let provider = row
        .try_get::<String, _>("provider")?
        .parse()
        .map_err(|error: SandboxAccountError| DbError::Contract(error.to_string()))?;
    let validation_status = row
        .try_get::<String, _>("validation_status")?
        .parse()
        .map_err(|error: SandboxAccountError| DbError::Contract(error.to_string()))?;
    Ok(SandboxAccount {
        id: row.try_get("id")?,
        provider,
        name: row.try_get("name")?,
        config: row.try_get("config")?,
        enabled: row.try_get("enabled")?,
        is_default: row.try_get("is_default")?,
        validation_status,
        last_validated_at: row
            .try_get::<Option<DateTime<Utc>>, _>("last_validated_at")?
            .map(timestamp),
        credential_version: row.try_get("credential_version")?,
        credentials_updated_at: timestamp(row.try_get("credentials_updated_at")?),
        created_at: timestamp(row.try_get("created_at")?),
        updated_at: timestamp(row.try_get("updated_at")?),
    })
}

fn validate_name(name: &str) -> Result<(), SandboxAccountError> {
    if name.is_empty() || name != name.trim() || name.chars().count() > 120 {
        return Err(SandboxAccountError::InvalidName);
    }
    Ok(())
}

fn validate_config(config: &Value) -> Result<(), SandboxAccountError> {
    if !config.is_object() {
        return Err(SandboxAccountError::ConfigMustBeObject);
    }
    Ok(())
}

fn validate_api_key(api_key: &str) -> Result<(), SandboxAccountError> {
    if api_key.is_empty() || api_key != api_key.trim() || api_key.len() > 65_536 {
        return Err(SandboxAccountError::InvalidApiKey);
    }
    Ok(())
}

fn secret_context(account_id: Uuid, secret_id: Uuid, provider: SandboxProvider) -> String {
    format!("execution-gateway:v1:sandbox-account:{account_id}:{secret_id}:{provider}")
}

fn timestamp(value: DateTime<Utc>) -> TimestampMs {
    TimestampMs(u64::try_from(value.timestamp_millis()).unwrap_or(0))
}

fn empty_object() -> Value {
    serde_json::json!({})
}

const fn default_true() -> bool {
    true
}

fn is_unique_violation(error: &DbError) -> bool {
    matches!(error, DbError::Sql(source) if source.as_database_error().is_some_and(|error| error.code().as_deref() == Some("23505")))
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
