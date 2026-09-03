use chrono::{DateTime, Utc};
use serde_json::Value;
use sqlx::{Postgres, Transaction};
use uuid::Uuid;

use super::{AdminAccount, Database, ProviderAccount, ResolvedAccount};
use crate::vault::EncryptedPayload;

pub struct NewProviderAccount {
    pub id: Uuid,
    pub provider: String,
    pub name: String,
    pub config: Value,
    pub status: String,
    pub make_default: bool,
    pub credential_expires_at: Option<DateTime<Utc>>,
    pub credential_refreshed_at: Option<DateTime<Utc>>,
}

#[derive(Default)]
pub struct AccountPatch {
    pub name: Option<String>,
    pub config: Option<Value>,
    pub status: Option<String>,
}

impl Database {
    pub async fn create_account(
        &self,
        account: NewProviderAccount,
        secret: EncryptedPayload,
    ) -> Result<ProviderAccount, AccountStoreError> {
        if account.make_default && account.status != "active" {
            return Err(AccountStoreError::InactiveCannotBeDefault);
        }
        let mut transaction = self.pool().begin().await?;
        lock_provider(&mut transaction, &account.provider).await?;
        let has_default: bool = sqlx::query_scalar(
            "select exists(select 1 from provider_accounts
             where provider = $1 and is_default and deleted_at is null)",
        )
        .bind(&account.provider)
        .fetch_one(&mut *transaction)
        .await?;
        let is_default = account.make_default || (account.status == "active" && !has_default);
        if account.make_default && has_default {
            sqlx::query(
                "update provider_accounts set is_default = false
                 where provider = $1 and is_default and deleted_at is null",
            )
            .bind(&account.provider)
            .execute(&mut *transaction)
            .await?;
        }
        let created = sqlx::query_as::<_, ProviderAccount>(
            "insert into provider_accounts (id, provider, name, config, status, is_default)
             values ($1, $2, $3, $4, $5, $6)
             returning id, provider, name, config, status, is_default, runtime_revision,
                       created_at, updated_at, deleted_at",
        )
        .bind(account.id)
        .bind(&account.provider)
        .bind(account.name)
        .bind(account.config)
        .bind(account.status)
        .bind(is_default)
        .fetch_one(&mut *transaction)
        .await?;
        sqlx::query(
            "insert into provider_credentials (
                provider_account_id, encrypted_payload, nonce, encryption_key_version,
                expires_at, refreshed_at
             ) values ($1, $2, $3, $4, $5, $6)",
        )
        .bind(account.id)
        .bind(secret.encrypted_payload)
        .bind(secret.nonce)
        .bind(secret.encryption_key_version)
        .bind(account.credential_expires_at)
        .bind(account.credential_refreshed_at)
        .execute(&mut *transaction)
        .await?;
        transaction.commit().await?;
        Ok(created)
    }

    pub async fn list_accounts(
        &self,
        provider: Option<&str>,
    ) -> Result<Vec<AdminAccount>, sqlx::Error> {
        const SELECT: &str = "select a.id, a.provider, a.name, a.config, a.status,
                    a.is_default, a.runtime_revision, a.created_at, a.updated_at,
                    c.credential_version, c.encryption_key_version,
                    c.expires_at as credential_expires_at,
                    c.refreshed_at as credential_refreshed_at,
                    c.updated_at as credential_updated_at
             from provider_accounts a
             join provider_credentials c on c.provider_account_id = a.id
             where a.deleted_at is null";
        match provider {
            Some(provider) => {
                sqlx::query_as(&format!(
                    "{SELECT} and a.provider = $1 order by a.name, a.id"
                ))
                .bind(provider)
                .fetch_all(self.pool())
                .await
            }
            None => {
                sqlx::query_as(&format!("{SELECT} order by a.provider, a.name, a.id"))
                    .fetch_all(self.pool())
                    .await
            }
        }
    }

    pub async fn find_account(
        &self,
        account_id: Uuid,
    ) -> Result<Option<ProviderAccount>, sqlx::Error> {
        sqlx::query_as(
            "select id, provider, name, config, status, is_default, runtime_revision,
                    created_at, updated_at, deleted_at
             from provider_accounts where id = $1 and deleted_at is null",
        )
        .bind(account_id)
        .fetch_optional(self.pool())
        .await
    }

    pub async fn find_admin_account(
        &self,
        account_id: Uuid,
    ) -> Result<Option<AdminAccount>, sqlx::Error> {
        sqlx::query_as(
            "select a.id, a.provider, a.name, a.config, a.status,
                    a.is_default, a.runtime_revision, a.created_at, a.updated_at,
                    c.credential_version, c.encryption_key_version,
                    c.expires_at as credential_expires_at,
                    c.refreshed_at as credential_refreshed_at,
                    c.updated_at as credential_updated_at
             from provider_accounts a
             join provider_credentials c on c.provider_account_id = a.id
             where a.id = $1 and a.deleted_at is null",
        )
        .bind(account_id)
        .fetch_optional(self.pool())
        .await
    }

    pub async fn update_account(
        &self,
        account_id: Uuid,
        patch: AccountPatch,
    ) -> Result<ProviderAccount, AccountStoreError> {
        let mut transaction = self.pool().begin().await?;
        let current = account_for_update(&mut transaction, account_id)
            .await?
            .ok_or(AccountStoreError::NotFound(account_id))?;
        if patch.status.as_deref() == Some("disabled") && current.is_default {
            return Err(AccountStoreError::DefaultCannotBeDisabled);
        }
        let config_changed = patch
            .config
            .as_ref()
            .is_some_and(|config| config != &current.config);
        let updated = sqlx::query_as::<_, ProviderAccount>(
            "update provider_accounts
             set name = coalesce($2, name),
                 config = coalesce($3, config),
                 status = coalesce($4, status),
                 runtime_revision = runtime_revision + case when $5 then 1 else 0 end
             where id = $1 and deleted_at is null
             returning id, provider, name, config, status, is_default, runtime_revision,
                       created_at, updated_at, deleted_at",
        )
        .bind(account_id)
        .bind(patch.name)
        .bind(patch.config)
        .bind(patch.status)
        .bind(config_changed)
        .fetch_optional(&mut *transaction)
        .await?
        .ok_or(AccountStoreError::NotFound(account_id))?;
        transaction.commit().await?;
        Ok(updated)
    }

    pub async fn resolve_account(
        &self,
        provider: &str,
        account_id: Option<Uuid>,
    ) -> Result<Option<ResolvedAccount>, sqlx::Error> {
        const SELECT: &str = "select a.id, a.provider, a.name, a.config, a.status,
                    a.is_default, a.runtime_revision, a.created_at, a.updated_at,
                    c.encrypted_payload, c.nonce, c.encryption_key_version,
                    c.credential_version, c.expires_at as credential_expires_at,
                    c.refreshed_at as credential_refreshed_at
             from provider_accounts a
             join provider_credentials c on c.provider_account_id = a.id
             where a.deleted_at is null";
        match account_id {
            Some(account_id) => {
                sqlx::query_as(&format!("{SELECT} and a.id = $1"))
                    .bind(account_id)
                    .fetch_optional(self.pool())
                    .await
            }
            None => {
                sqlx::query_as(&format!("{SELECT} and a.provider = $1 and a.is_default"))
                    .bind(provider)
                    .fetch_optional(self.pool())
                    .await
            }
        }
    }

    pub async fn set_default_account(
        &self,
        account_id: Uuid,
    ) -> Result<ProviderAccount, AccountStoreError> {
        let mut transaction = self.pool().begin().await?;
        let account = account_for_update(&mut transaction, account_id)
            .await?
            .ok_or(AccountStoreError::NotFound(account_id))?;
        if account.status != "active" {
            return Err(AccountStoreError::InactiveCannotBeDefault);
        }
        lock_provider(&mut transaction, &account.provider).await?;
        sqlx::query(
            "update provider_accounts set is_default = false
             where provider = $1 and is_default and deleted_at is null",
        )
        .bind(&account.provider)
        .execute(&mut *transaction)
        .await?;
        let updated = sqlx::query_as::<_, ProviderAccount>(
            "update provider_accounts set is_default = true
             where id = $1 and deleted_at is null
             returning id, provider, name, config, status, is_default, runtime_revision,
                       created_at, updated_at, deleted_at",
        )
        .bind(account_id)
        .fetch_one(&mut *transaction)
        .await?;
        transaction.commit().await?;
        Ok(updated)
    }

    pub async fn rotate_credentials(
        &self,
        account_id: Uuid,
        secret: EncryptedPayload,
        expires_at: Option<DateTime<Utc>>,
        refreshed_at: Option<DateTime<Utc>>,
    ) -> Result<i64, AccountStoreError> {
        let mut transaction = self.pool().begin().await?;
        let version: Option<i64> = sqlx::query_scalar(
            "update provider_credentials c
             set encrypted_payload = $2, nonce = $3, encryption_key_version = $4,
                 credential_version = credential_version + 1,
                 expires_at = $5, refreshed_at = $6
             from provider_accounts a
             where a.id = $1 and a.deleted_at is null and c.provider_account_id = a.id
             returning c.credential_version",
        )
        .bind(account_id)
        .bind(secret.encrypted_payload)
        .bind(secret.nonce)
        .bind(secret.encryption_key_version)
        .bind(expires_at)
        .bind(refreshed_at)
        .fetch_optional(&mut *transaction)
        .await?;
        let version = version.ok_or(AccountStoreError::NotFound(account_id))?;
        sqlx::query(
            "update provider_accounts
             set status = 'active', runtime_revision = runtime_revision + 1
             where id = $1 and deleted_at is null",
        )
        .bind(account_id)
        .execute(&mut *transaction)
        .await?;
        transaction.commit().await?;
        Ok(version)
    }

    pub async fn mark_reauthentication_required(
        &self,
        account_id: Uuid,
    ) -> Result<(), sqlx::Error> {
        sqlx::query(
            "update provider_accounts set status = 'reauth_required'
             where id = $1 and deleted_at is null",
        )
        .bind(account_id)
        .execute(self.pool())
        .await?;
        Ok(())
    }

    pub async fn remove_account(&self, account_id: Uuid) -> Result<(), AccountStoreError> {
        let mut transaction = self.pool().begin().await?;
        let removed = sqlx::query_scalar::<_, Uuid>(
            "update provider_accounts
             set status = 'disabled', is_default = false, deleted_at = now(),
                 runtime_revision = runtime_revision + 1
             where id = $1 and deleted_at is null returning id",
        )
        .bind(account_id)
        .fetch_optional(&mut *transaction)
        .await?;
        removed.ok_or(AccountStoreError::NotFound(account_id))?;
        sqlx::query("delete from provider_credentials where provider_account_id = $1")
            .bind(account_id)
            .execute(&mut *transaction)
            .await?;
        transaction.commit().await?;
        Ok(())
    }
}

async fn lock_provider(
    transaction: &mut Transaction<'_, Postgres>,
    provider: &str,
) -> Result<(), sqlx::Error> {
    sqlx::query("select pg_advisory_xact_lock(hashtextextended($1, 0))")
        .bind(provider)
        .execute(&mut **transaction)
        .await?;
    Ok(())
}

async fn account_for_update(
    transaction: &mut Transaction<'_, Postgres>,
    account_id: Uuid,
) -> Result<Option<ProviderAccount>, sqlx::Error> {
    sqlx::query_as(
        "select id, provider, name, config, status, is_default, runtime_revision,
                created_at, updated_at, deleted_at
         from provider_accounts where id = $1 and deleted_at is null for update",
    )
    .bind(account_id)
    .fetch_optional(&mut **transaction)
    .await
}

#[derive(Debug, thiserror::Error)]
pub enum AccountStoreError {
    #[error("provider account {0} was not found")]
    NotFound(Uuid),
    #[error("only an active provider account can be the default")]
    InactiveCannotBeDefault,
    #[error("the default provider account cannot be disabled; select another default first")]
    DefaultCannotBeDisabled,
    #[error(transparent)]
    Database(#[from] sqlx::Error),
}
