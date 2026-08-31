use chrono::{DateTime, Utc};
use credential_vault::{CredentialVault, EncryptedSecret, VaultError};
use execution_contracts::{
    EnvironmentId, MachineDescriptor, MachineId, TimestampMs, WorkspaceRootId,
};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use sqlx::Row;
use thiserror::Error;
use uuid::Uuid;
use zeroize::Zeroizing;

use crate::{
    db::{Database, DbError},
    sandbox_accounts::SandboxProvider,
    sandbox_templates::SandboxEnvironmentInstance,
};

#[derive(Clone, Debug)]
pub struct SandboxMachine {
    pub machine_id: MachineId,
    pub sandbox_account_id: Uuid,
    pub provider: SandboxProvider,
    pub provider_resource_id: String,
    pub connection_secret_id: Option<Uuid>,
    pub connection_config: Value,
    pub provider_metadata: Value,
    pub account_config: Value,
}

#[derive(Clone, Debug)]
pub struct SandboxEnvironmentResource {
    pub environment_id: EnvironmentId,
    pub machine_id: MachineId,
    pub sandbox_account_id: Uuid,
    pub provider: SandboxProvider,
    pub provider_resource_id: String,
    pub account_config: Value,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct SandboxMachineSummary {
    pub machine_id: MachineId,
    pub sandbox_account_id: Uuid,
    pub sandbox_id: String,
    pub name: String,
    pub created_from: Option<String>,
    pub created_at: TimestampMs,
}

pub struct NewSandboxMachine<'a> {
    pub machine_id: &'a MachineId,
    pub name: &'a str,
    pub descriptor: &'a MachineDescriptor,
    pub sandbox_account_id: Uuid,
    pub provider_resource_id: &'a str,
    pub connection_config: &'a Value,
    pub provider_metadata: &'a Value,
    pub connection_secret: Option<(Uuid, EncryptedSecret)>,
}

impl Database {
    pub async fn create_sandbox_machine(
        &self,
        machine: NewSandboxMachine<'_>,
    ) -> Result<(), DbError> {
        let mut transaction = self.pool().begin().await?;
        let connection_secret_id = machine.connection_secret.as_ref().map(|(id, _)| *id);
        if let Some((secret_id, secret)) = machine.connection_secret {
            sqlx::query(
                "insert into execution_vault_secrets
                     (id, encrypted_payload, nonce, encryption_key_version)
                 values ($1, $2, $3, $4)",
            )
            .bind(secret_id)
            .bind(secret.encrypted_payload)
            .bind(secret.nonce)
            .bind(secret.encryption_key_version)
            .execute(&mut *transaction)
            .await?;
        }
        sqlx::query(
            "insert into machines
                 (machine_id, name, connector_kind, descriptor, credential_hash)
             values ($1, $2, 'sandbox', $3, null)",
        )
        .bind(machine.machine_id.as_str())
        .bind(machine.name)
        .bind(serde_json::to_value(machine.descriptor)?)
        .execute(&mut *transaction)
        .await?;
        sqlx::query(
            "insert into sandbox_machines
                 (machine_id, sandbox_account_id, provider_resource_id,
                  connection_secret_id, connection_config, provider_metadata)
             values ($1, $2, $3, $4, $5, $6)",
        )
        .bind(machine.machine_id.as_str())
        .bind(machine.sandbox_account_id)
        .bind(machine.provider_resource_id)
        .bind(connection_secret_id)
        .bind(machine.connection_config)
        .bind(machine.provider_metadata)
        .execute(&mut *transaction)
        .await?;
        transaction.commit().await?;
        Ok(())
    }

    pub async fn complete_sandbox_materialization(
        &self,
        machine_id: &MachineId,
        environment_id: &EnvironmentId,
        environment_name: &str,
        workspace_root_id: &WorkspaceRootId,
        path: &str,
        template_id: Uuid,
    ) -> Result<SandboxEnvironmentInstance, DbError> {
        let mut transaction = self.pool().begin().await?;
        sqlx::query(
            "insert into environments
                 (environment_id, project_id, machine_id, name, workspace_root_id, path)
             select $1, t.project_id, $2, $3, $4, $5
             from sandbox_environment_templates t
             where t.id = $6 and t.deleted_at is null",
        )
        .bind(environment_id.as_str())
        .bind(machine_id.as_str())
        .bind(environment_name)
        .bind(workspace_root_id.as_str())
        .bind(path)
        .bind(template_id)
        .execute(&mut *transaction)
        .await?;
        sqlx::query(
            "insert into sandbox_environment_instances (environment_id, template_id)
             values ($1, $2)",
        )
        .bind(environment_id.as_str())
        .bind(template_id)
        .execute(&mut *transaction)
        .await?;
        transaction.commit().await?;
        self.sandbox_environment_instance(environment_id.as_str())
            .await?
            .ok_or_else(|| DbError::Contract("materialized environment disappeared".to_owned()))
    }

    pub async fn fail_sandbox_materialization(
        &self,
        machine_id: &MachineId,
        message: &str,
    ) -> Result<(), DbError> {
        let mut transaction = self.pool().begin().await?;
        let secret_id: Option<Uuid> = sqlx::query_scalar(
            "select connection_secret_id from sandbox_machines where machine_id = $1",
        )
        .bind(machine_id.as_str())
        .fetch_optional(&mut *transaction)
        .await?
        .flatten();
        sqlx::query(
            "update sandbox_machines
             set last_error = jsonb_build_object('message', $2), connection_secret_id = null
             where machine_id = $1",
        )
        .bind(machine_id.as_str())
        .bind(message)
        .execute(&mut *transaction)
        .await?;
        sqlx::query(
            "update machines set deleted_at = now(), updated_at = now() where machine_id = $1",
        )
        .bind(machine_id.as_str())
        .execute(&mut *transaction)
        .await?;
        if let Some(secret_id) = secret_id {
            sqlx::query("delete from execution_vault_secrets where id = $1")
                .bind(secret_id)
                .execute(&mut *transaction)
                .await?;
        }
        transaction.commit().await?;
        Ok(())
    }

    pub async fn sandbox_machine(
        &self,
        machine_id: &str,
    ) -> Result<Option<SandboxMachine>, DbError> {
        sqlx::query(
            "select sm.machine_id, sm.sandbox_account_id, a.provider,
                    sm.provider_resource_id, sm.connection_secret_id,
                    sm.connection_config, sm.provider_metadata, a.config as account_config
             from sandbox_machines sm
             join sandbox_accounts a on a.id = sm.sandbox_account_id
             join machines m on m.machine_id = sm.machine_id
             where sm.machine_id = $1 and m.deleted_at is null
               and a.deleted_at is null and a.enabled",
        )
        .bind(machine_id)
        .fetch_optional(self.pool())
        .await?
        .map(sandbox_machine_from_row)
        .transpose()
    }

    pub async fn sandbox_machines_for_account(
        &self,
        account_id: Uuid,
    ) -> Result<Vec<SandboxMachineSummary>, DbError> {
        sqlx::query(
            "select m.machine_id, sm.sandbox_account_id,
                    sm.provider_resource_id as sandbox_id, m.name,
                    coalesce(
                        nullif(sm.provider_metadata ->> 'created_from', ''),
                        nullif(sm.provider_metadata ->> 'template_id', '')
                    ) as created_from,
                    m.created_at
             from sandbox_machines sm
             join machines m on m.machine_id = sm.machine_id
             where sm.sandbox_account_id = $1 and m.deleted_at is null
             order by m.created_at desc, m.machine_id",
        )
        .bind(account_id)
        .fetch_all(self.pool())
        .await?
        .into_iter()
        .map(sandbox_machine_summary_from_row)
        .collect()
    }

    pub async fn has_sandbox_machine(
        &self,
        account_id: Uuid,
        provider_resource_id: &str,
    ) -> Result<bool, DbError> {
        sqlx::query_scalar(
            "select exists(
                 select 1
                 from sandbox_machines sm
                 join machines m on m.machine_id = sm.machine_id
                 where sm.sandbox_account_id = $1
                   and sm.provider_resource_id = $2
                   and m.deleted_at is null
             )",
        )
        .bind(account_id)
        .bind(provider_resource_id)
        .fetch_one(self.pool())
        .await
        .map_err(Into::into)
    }

    pub async fn sandbox_environment_resource(
        &self,
        environment_id: &str,
    ) -> Result<Option<SandboxEnvironmentResource>, DbError> {
        sqlx::query(
            "select e.environment_id, e.machine_id, sm.sandbox_account_id, a.provider,
                    sm.provider_resource_id, a.config as account_config
             from sandbox_environment_instances i
             join environments e on e.environment_id = i.environment_id
             join sandbox_machines sm on sm.machine_id = e.machine_id
             join sandbox_accounts a on a.id = sm.sandbox_account_id
             where e.environment_id = $1 and e.deleted_at is null",
        )
        .bind(environment_id)
        .fetch_optional(self.pool())
        .await?
        .map(sandbox_resource_from_row)
        .transpose()
    }

    pub async fn delete_materialized_sandbox_environment(
        &self,
        resource: &SandboxEnvironmentResource,
    ) -> Result<bool, DbError> {
        let mut transaction = self.pool().begin().await?;
        let deleted = sqlx::query(
            "update environments set deleted_at = now()
             where environment_id = $1 and deleted_at is null",
        )
        .bind(resource.environment_id.as_str())
        .execute(&mut *transaction)
        .await?;
        if deleted.rows_affected() == 0 {
            return Ok(false);
        }
        let secret_id: Option<Uuid> = sqlx::query_scalar(
            "select connection_secret_id from sandbox_machines where machine_id = $1",
        )
        .bind(resource.machine_id.as_str())
        .fetch_optional(&mut *transaction)
        .await?
        .flatten();
        sqlx::query(
            "update sandbox_machines set connection_secret_id = null where machine_id = $1",
        )
        .bind(resource.machine_id.as_str())
        .execute(&mut *transaction)
        .await?;
        sqlx::query(
            "update machines set deleted_at = now(), updated_at = now() where machine_id = $1",
        )
        .bind(resource.machine_id.as_str())
        .execute(&mut *transaction)
        .await?;
        if let Some(secret_id) = secret_id {
            sqlx::query("delete from execution_vault_secrets where id = $1")
                .bind(secret_id)
                .execute(&mut *transaction)
                .await?;
        }
        transaction.commit().await?;
        Ok(true)
    }

    async fn sandbox_environment_instance(
        &self,
        environment_id: &str,
    ) -> Result<Option<SandboxEnvironmentInstance>, DbError> {
        self.sandbox_environment_instances_for_environment(environment_id)
            .await
    }

    async fn sandbox_environment_instances_for_environment(
        &self,
        environment_id: &str,
    ) -> Result<Option<SandboxEnvironmentInstance>, DbError> {
        let row = sqlx::query_scalar::<_, Uuid>(
            "select template_id from sandbox_environment_instances where environment_id = $1",
        )
        .bind(environment_id)
        .fetch_optional(self.pool())
        .await?;
        let Some(template_id) = row else {
            return Ok(None);
        };
        Ok(self
            .sandbox_environment_instances(template_id)
            .await?
            .into_iter()
            .find(|instance| instance.environment.environment_id.as_str() == environment_id))
    }

    async fn encrypted_connection_secret(
        &self,
        secret_id: Uuid,
    ) -> Result<Option<StoredConnectionSecret>, DbError> {
        sqlx::query(
            "select encrypted_payload, nonce, encryption_key_version
             from execution_vault_secrets where id = $1",
        )
        .bind(secret_id)
        .fetch_optional(self.pool())
        .await?
        .map(|row| {
            Ok(StoredConnectionSecret {
                encrypted_payload: row.try_get("encrypted_payload")?,
                nonce: row.try_get("nonce")?,
                encryption_key_version: row.try_get("encryption_key_version")?,
            })
        })
        .transpose()
    }
}

#[derive(Clone)]
pub struct SandboxSecretStore {
    database: Database,
    vault: CredentialVault,
}

impl SandboxSecretStore {
    pub const fn new(database: Database, vault: CredentialVault) -> Self {
        Self { database, vault }
    }

    pub fn encrypt(
        &self,
        machine_id: &MachineId,
        plaintext: &str,
    ) -> Result<(Uuid, EncryptedSecret), VaultError> {
        let secret_id = Uuid::now_v7();
        let encrypted = self.vault.encrypt(
            &connection_secret_context(machine_id, secret_id),
            plaintext.as_bytes(),
        )?;
        Ok((secret_id, encrypted))
    }

    pub async fn decrypt(
        &self,
        machine_id: &MachineId,
        secret_id: Option<Uuid>,
    ) -> Result<Option<Zeroizing<String>>, SandboxSecretError> {
        let Some(secret_id) = secret_id else {
            return Ok(None);
        };
        let stored = self
            .database
            .encrypted_connection_secret(secret_id)
            .await?
            .ok_or(SandboxSecretError::Missing)?;
        let bytes = self.vault.decrypt(
            &connection_secret_context(machine_id, secret_id),
            &stored.encrypted_payload,
            &stored.nonce,
            stored.encryption_key_version,
        )?;
        String::from_utf8(bytes.to_vec())
            .map(Zeroizing::new)
            .map(Some)
            .map_err(|_| SandboxSecretError::InvalidUtf8)
    }
}

struct StoredConnectionSecret {
    encrypted_payload: Vec<u8>,
    nonce: Vec<u8>,
    encryption_key_version: i32,
}

#[derive(Debug, Error)]
pub enum SandboxSecretError {
    #[error("sandbox connection secret is missing")]
    Missing,
    #[error("sandbox connection secret is not UTF-8")]
    InvalidUtf8,
    #[error(transparent)]
    Database(#[from] DbError),
    #[error(transparent)]
    Vault(#[from] VaultError),
}

fn connection_secret_context(machine_id: &MachineId, secret_id: Uuid) -> String {
    format!("execution-gateway:v1:sandbox-machine:{machine_id}:{secret_id}")
}

fn sandbox_machine_from_row(row: sqlx::postgres::PgRow) -> Result<SandboxMachine, DbError> {
    Ok(SandboxMachine {
        machine_id: MachineId::new(row.try_get::<String, _>("machine_id")?)
            .map_err(|error| DbError::Contract(error.to_string()))?,
        sandbox_account_id: row.try_get("sandbox_account_id")?,
        provider: provider_from_row(&row)?,
        provider_resource_id: row.try_get("provider_resource_id")?,
        connection_secret_id: row.try_get("connection_secret_id")?,
        connection_config: row.try_get("connection_config")?,
        provider_metadata: row.try_get("provider_metadata")?,
        account_config: row.try_get("account_config")?,
    })
}

fn sandbox_resource_from_row(
    row: sqlx::postgres::PgRow,
) -> Result<SandboxEnvironmentResource, DbError> {
    Ok(SandboxEnvironmentResource {
        environment_id: EnvironmentId::new(row.try_get::<String, _>("environment_id")?)
            .map_err(|error| DbError::Contract(error.to_string()))?,
        machine_id: MachineId::new(row.try_get::<String, _>("machine_id")?)
            .map_err(|error| DbError::Contract(error.to_string()))?,
        sandbox_account_id: row.try_get("sandbox_account_id")?,
        provider: provider_from_row(&row)?,
        provider_resource_id: row.try_get("provider_resource_id")?,
        account_config: row.try_get("account_config")?,
    })
}

fn sandbox_machine_summary_from_row(
    row: sqlx::postgres::PgRow,
) -> Result<SandboxMachineSummary, DbError> {
    Ok(SandboxMachineSummary {
        machine_id: MachineId::new(row.try_get::<String, _>("machine_id")?)
            .map_err(|error| DbError::Contract(error.to_string()))?,
        sandbox_account_id: row.try_get("sandbox_account_id")?,
        sandbox_id: row.try_get("sandbox_id")?,
        name: row.try_get("name")?,
        created_from: row.try_get("created_from")?,
        created_at: timestamp(row.try_get("created_at")?),
    })
}

fn provider_from_row(row: &sqlx::postgres::PgRow) -> Result<SandboxProvider, DbError> {
    row.try_get::<String, _>("provider")?.parse().map_err(
        |error: crate::sandbox_accounts::SandboxAccountError| DbError::Contract(error.to_string()),
    )
}

fn timestamp(value: DateTime<Utc>) -> TimestampMs {
    TimestampMs(u64::try_from(value.timestamp_millis()).unwrap_or(0))
}
