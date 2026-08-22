use serde_json::Value;
use sqlx::{Postgres, Transaction};
use uuid::Uuid;

use super::{AdminAccount, Database, ProviderAccount, ResolvedAccount};
use crate::vault::EncryptedPayload;

pub struct NewProviderAccount {
    pub id: Uuid,
    pub provider: String,
    pub name: String,
    pub secret_id: Uuid,
    pub config: Value,
    pub enabled: bool,
    pub make_default: bool,
}

#[derive(Default)]
pub struct AccountPatch {
    pub name: Option<String>,
    pub config: Option<Value>,
    pub enabled: Option<bool>,
}

impl Database {
    pub async fn create_account(
        &self,
        account: NewProviderAccount,
        secret: EncryptedPayload,
    ) -> Result<ProviderAccount, AccountStoreError> {
        if account.make_default && !account.enabled {
            return Err(AccountStoreError::DisabledCannotBeDefault);
        }

        let mut transaction = self.pool().begin().await?;
        lock_provider(&mut transaction, &account.provider).await?;

        let has_default: bool = sqlx::query_scalar(
            "select exists(
                select 1 from provider_accounts
                where provider = $1 and is_default
            )",
        )
        .bind(&account.provider)
        .fetch_one(&mut *transaction)
        .await?;
        let is_default = account.make_default || (account.enabled && !has_default);

        if is_default && has_default {
            sqlx::query(
                "update provider_accounts
                 set is_default = false
                 where provider = $1 and is_default",
            )
            .bind(&account.provider)
            .execute(&mut *transaction)
            .await?;
        }

        sqlx::query(
            "insert into vault_secrets (
                id, encrypted_payload, nonce, encryption_key_version
             ) values ($1, $2, $3, $4)",
        )
        .bind(account.secret_id)
        .bind(secret.encrypted_payload)
        .bind(secret.nonce)
        .bind(secret.encryption_key_version)
        .execute(&mut *transaction)
        .await?;

        let created = sqlx::query_as::<_, ProviderAccount>(
            "insert into provider_accounts (
                id, provider, name, secret_id, config, enabled, is_default
             ) values ($1, $2, $3, $4, $5, $6, $7)
             returning id, provider, name, secret_id, config, enabled, is_default,
                       created_at, updated_at",
        )
        .bind(account.id)
        .bind(&account.provider)
        .bind(&account.name)
        .bind(account.secret_id)
        .bind(account.config)
        .bind(account.enabled)
        .bind(is_default)
        .fetch_one(&mut *transaction)
        .await?;

        transaction.commit().await?;
        Ok(created)
    }

    pub async fn list_accounts(
        &self,
        provider: Option<&str>,
        enabled_only: bool,
    ) -> Result<Vec<ProviderAccount>, sqlx::Error> {
        let base = "select id, provider, name, secret_id, config, enabled, is_default,
                           created_at, updated_at
                    from provider_accounts";
        match (provider, enabled_only) {
            (Some(provider), true) => {
                sqlx::query_as(&format!(
                    "{base} where provider = $1 and enabled order by name, id"
                ))
                .bind(provider)
                .fetch_all(self.pool())
                .await
            }
            (Some(provider), false) => {
                sqlx::query_as(&format!("{base} where provider = $1 order by name, id"))
                    .bind(provider)
                    .fetch_all(self.pool())
                    .await
            }
            (None, true) => {
                sqlx::query_as(&format!("{base} where enabled order by provider, name, id"))
                    .fetch_all(self.pool())
                    .await
            }
            (None, false) => {
                sqlx::query_as(&format!("{base} order by provider, name, id"))
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
            "select id, provider, name, secret_id, config, enabled, is_default,
                    created_at, updated_at
             from provider_accounts
             where id = $1",
        )
        .bind(account_id)
        .fetch_optional(self.pool())
        .await
    }

    pub async fn list_admin_accounts(
        &self,
        provider: Option<&str>,
    ) -> Result<Vec<AdminAccount>, sqlx::Error> {
        const SELECT: &str = "select a.id, a.provider, a.name, a.config, a.enabled, a.is_default,
                    a.created_at, a.updated_at, s.credential_version,
                    s.encryption_key_version, s.updated_at as credential_updated_at
             from provider_accounts a
             join vault_secrets s on s.id = a.secret_id";
        match provider {
            Some(provider) => {
                sqlx::query_as(&format!(
                    "{SELECT} where a.provider = $1 order by a.name, a.id"
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

    pub async fn find_admin_account(
        &self,
        account_id: Uuid,
    ) -> Result<Option<AdminAccount>, sqlx::Error> {
        sqlx::query_as(
            "select a.id, a.provider, a.name, a.config, a.enabled, a.is_default,
                    a.created_at, a.updated_at, s.credential_version,
                    s.encryption_key_version, s.updated_at as credential_updated_at
             from provider_accounts a
             join vault_secrets s on s.id = a.secret_id
             where a.id = $1",
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
        if patch.enabled == Some(false) && current.is_default {
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
                 enabled = coalesce($4, enabled)
             where id = $1
             returning id, provider, name, secret_id, config, enabled, is_default,
                       created_at, updated_at",
        )
        .bind(account_id)
        .bind(patch.name)
        .bind(patch.config)
        .bind(patch.enabled)
        .fetch_one(&mut *transaction)
        .await?;

        if config_changed {
            sqlx::query(
                "update vault_secrets
                 set credential_version = credential_version + 1
                 where id = $1",
            )
            .bind(updated.secret_id)
            .execute(&mut *transaction)
            .await?;
        }

        transaction.commit().await?;
        Ok(updated)
    }

    pub async fn resolve_account(
        &self,
        provider: &str,
        account_id: Option<Uuid>,
    ) -> Result<Option<ResolvedAccount>, sqlx::Error> {
        const SELECT: &str = "select a.id, a.provider, a.name, a.secret_id, a.config, a.enabled,
                    a.is_default, a.created_at, a.updated_at,
                    s.encrypted_payload, s.nonce, s.encryption_key_version,
                    s.credential_version
             from provider_accounts a
             join vault_secrets s on s.id = a.secret_id";

        match account_id {
            Some(account_id) => {
                sqlx::query_as(&format!("{SELECT} where a.id = $1"))
                    .bind(account_id)
                    .fetch_optional(self.pool())
                    .await
            }
            None => {
                sqlx::query_as(&format!(
                    "{SELECT} where a.provider = $1 and a.enabled and a.is_default"
                ))
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
        let account = self
            .find_account(account_id)
            .await?
            .ok_or(AccountStoreError::NotFound(account_id))?;
        if !account.enabled {
            return Err(AccountStoreError::DisabledCannotBeDefault);
        }
        let mut transaction = self.pool().begin().await?;
        lock_provider(&mut transaction, &account.provider).await?;
        let account = account_for_update(&mut transaction, account_id)
            .await?
            .ok_or(AccountStoreError::NotFound(account_id))?;
        if !account.enabled {
            return Err(AccountStoreError::DisabledCannotBeDefault);
        }

        sqlx::query(
            "update provider_accounts
             set is_default = false
             where provider = $1 and is_default",
        )
        .bind(&account.provider)
        .execute(&mut *transaction)
        .await?;

        let updated = sqlx::query_as::<_, ProviderAccount>(
            "update provider_accounts
             set is_default = true
             where id = $1
             returning id, provider, name, secret_id, config, enabled, is_default,
                       created_at, updated_at",
        )
        .bind(account_id)
        .fetch_one(&mut *transaction)
        .await?;

        transaction.commit().await?;
        Ok(updated)
    }

    pub async fn set_account_enabled(
        &self,
        account_id: Uuid,
        enabled: bool,
    ) -> Result<ProviderAccount, AccountStoreError> {
        let account = self
            .find_account(account_id)
            .await?
            .ok_or(AccountStoreError::NotFound(account_id))?;
        if !enabled && account.is_default {
            return Err(AccountStoreError::DefaultCannotBeDisabled);
        }

        sqlx::query_as(
            "update provider_accounts
             set enabled = $2
             where id = $1
             returning id, provider, name, secret_id, config, enabled, is_default,
                       created_at, updated_at",
        )
        .bind(account_id)
        .bind(enabled)
        .fetch_one(self.pool())
        .await
        .map_err(Into::into)
    }

    pub async fn set_account_config(
        &self,
        account_id: Uuid,
        config: Value,
    ) -> Result<ProviderAccount, AccountStoreError> {
        let mut transaction = self.pool().begin().await?;
        let updated = sqlx::query_as::<_, ProviderAccount>(
            "update provider_accounts
             set config = $2
             where id = $1
             returning id, provider, name, secret_id, config, enabled, is_default,
                       created_at, updated_at",
        )
        .bind(account_id)
        .bind(config)
        .fetch_optional(&mut *transaction)
        .await?;
        let updated = updated.ok_or(AccountStoreError::NotFound(account_id))?;
        sqlx::query(
            "update vault_secrets
             set credential_version = credential_version + 1
             where id = $1",
        )
        .bind(updated.secret_id)
        .execute(&mut *transaction)
        .await?;
        transaction.commit().await?;
        Ok(updated)
    }

    pub async fn rotate_credentials(
        &self,
        account_id: Uuid,
        secret: EncryptedPayload,
    ) -> Result<i64, AccountStoreError> {
        let version = sqlx::query_scalar(
            "update vault_secrets s
             set encrypted_payload = $2,
                 nonce = $3,
                 encryption_key_version = $4,
                 credential_version = credential_version + 1
             from provider_accounts a
             where a.id = $1 and a.secret_id = s.id
             returning s.credential_version",
        )
        .bind(account_id)
        .bind(secret.encrypted_payload)
        .bind(secret.nonce)
        .bind(secret.encryption_key_version)
        .fetch_optional(self.pool())
        .await?;
        version.ok_or(AccountStoreError::NotFound(account_id))
    }

    pub async fn remove_account(&self, account_id: Uuid) -> Result<(), AccountStoreError> {
        let mut transaction = self.pool().begin().await?;
        let secret_id: Option<Uuid> =
            sqlx::query_scalar("delete from provider_accounts where id = $1 returning secret_id")
                .bind(account_id)
                .fetch_optional(&mut *transaction)
                .await?;
        let secret_id = secret_id.ok_or(AccountStoreError::NotFound(account_id))?;
        sqlx::query("delete from vault_secrets where id = $1")
            .bind(secret_id)
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
        "select id, provider, name, secret_id, config, enabled, is_default,
                created_at, updated_at
         from provider_accounts
         where id = $1
         for update",
    )
    .bind(account_id)
    .fetch_optional(&mut **transaction)
    .await
}

#[derive(Debug, thiserror::Error)]
pub enum AccountStoreError {
    #[error("provider account {0} was not found")]
    NotFound(Uuid),
    #[error("a disabled provider account cannot be the default")]
    DisabledCannotBeDefault,
    #[error("the default provider account cannot be disabled; select another default first")]
    DefaultCannotBeDisabled,
    #[error(transparent)]
    Database(#[from] sqlx::Error),
}
