use std::time::Duration;

use execution_api::{
    DesiredHostState, DesiredSnapshotState, E2bAccount, E2bAccountStatus, E2bHostBinding,
    E2bHostSource, E2bSnapshot, ExecutionHost, ExecutionHostKind, ExecutionHostState,
    RegisteredHostBinding, SnapshotState,
};
use execution_core::{ExecutionHostDescriptor, ExecutionRoot};
use execution_e2b::SupervisorConfig;
use serde_json::Value;
use sqlx::{Error as SqlxError, PgPool, Postgres, Row, Transaction, postgres::PgPoolOptions};
use thiserror::Error;
use uuid::Uuid;

const HOST_SELECT: &str = r#"
select h.*, e.e2b_account_id, e.e2b_sandbox_id, e.source_type,
       e.source_snapshot_id, e.timeout_seconds, e.network_access, e.ram_mb,
       r.installation_id, r.daemon_version, r.protocol_version,
       r.registered_at, r.last_connected_at, r.last_disconnected_at
from execution_hosts h
left join e2b_hosts e on e.host_id = h.id
left join registered_hosts r on r.host_id = h.id
"#;

const SNAPSHOT_SELECT: &str = "select * from e2b_snapshots";

#[derive(Clone)]
pub struct Database {
    pool: PgPool,
}

#[derive(Debug, Error)]
pub enum DatabaseError {
    #[error(transparent)]
    Sql(#[from] sqlx::Error),
    #[error("stored gateway data is invalid: {0}")]
    InvalidData(String),
    #[error("the idempotency key was already used with a different request")]
    IdempotencyConflict,
    #[error("the idempotency record does not reference the expected resource")]
    InvalidIdempotencyRecord,
    #[error("host is not ready for snapshotting")]
    SnapshotHostNotReady,
    #[error("registered hosts do not support snapshots")]
    SnapshotUnsupported,
    #[error("execution host not found")]
    HostNotFound,
}

#[derive(Clone, Debug)]
pub struct StoredCredential {
    pub account_id: Uuid,
    pub ciphertext: Vec<u8>,
    pub nonce: Vec<u8>,
    pub key_version: i32,
}

#[derive(Clone, Debug)]
pub struct SnapshotSource {
    pub snapshot_id: Uuid,
    pub e2b_account_id: Uuid,
    pub e2b_snapshot_id: String,
}

#[derive(Clone, Debug)]
pub struct HostWork {
    pub host_id: Uuid,
    pub desired_state: DesiredHostState,
    pub observed_state: ExecutionHostState,
    pub e2b_account_id: Uuid,
    pub account_status: E2bAccountStatus,
    pub e2b_sandbox_id: Option<String>,
    pub source_type: String,
    pub source_snapshot_provider_id: Option<String>,
    pub timeout_seconds: u64,
    pub network_access: bool,
    pub ram_mb: Option<u32>,
    pub credential: StoredCredential,
}

#[derive(Clone, Copy, Debug)]
pub struct HostFailure<'a> {
    pub observed_state: &'a str,
    pub code: &'a str,
    pub message: &'a str,
    pub retryable: bool,
    pub ambiguous: bool,
}

#[derive(Clone, Debug)]
pub struct SnapshotWork {
    pub id: Uuid,
    pub desired_state: DesiredSnapshotState,
    pub e2b_snapshot_id: Option<String>,
    pub source_host_id: Option<Uuid>,
    pub source_sandbox_id: Option<String>,
    pub timeout_seconds: Option<u64>,
    pub credential: StoredCredential,
    pub account_status: E2bAccountStatus,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct IdempotentResource {
    pub id: Uuid,
    pub replayed: bool,
}

#[derive(Clone, Debug, Default)]
pub struct HostListFilter {
    pub state: Option<String>,
    pub kind: Option<String>,
    pub e2b_account_id: Option<Uuid>,
    pub include_deleted: bool,
}

#[derive(Clone, Debug, Default)]
pub struct SnapshotListFilter {
    pub state: Option<String>,
    pub e2b_account_id: Option<Uuid>,
    pub source_host_id: Option<Uuid>,
    pub include_deleted: bool,
}

impl Database {
    pub async fn connect(url: &str, max_connections: u32) -> Result<Self, sqlx::Error> {
        let pool = PgPoolOptions::new()
            .max_connections(max_connections)
            .acquire_timeout(Duration::from_secs(10))
            .connect(url)
            .await?;
        Ok(Self { pool })
    }

    pub async fn migrate(&self) -> Result<(), sqlx::migrate::MigrateError> {
        sqlx::migrate!("./migrations").run(&self.pool).await
    }

    pub async fn health(&self) -> Result<(), sqlx::Error> {
        sqlx::query("select 1").execute(&self.pool).await?;
        Ok(())
    }

    #[allow(clippy::too_many_arguments)]
    pub async fn create_account(
        &self,
        id: Uuid,
        name: &str,
        fingerprint: &str,
        ciphertext: &[u8],
        nonce: &[u8],
        key_version: i32,
        is_default: bool,
    ) -> Result<E2bAccount, DatabaseError> {
        let mut transaction = self.pool.begin().await?;
        lock_accounts(&mut transaction).await?;
        let any_account: bool = sqlx::query_scalar("select exists(select 1 from e2b_accounts)")
            .fetch_one(&mut *transaction)
            .await?;
        let make_default = is_default || !any_account;
        if make_default {
            sqlx::query("update e2b_accounts set is_default = false where is_default")
                .execute(&mut *transaction)
                .await?;
        }
        sqlx::query(
            "insert into e2b_accounts (
                id, name, credential_ciphertext, credential_nonce, credential_key_version,
                credential_fingerprint, is_default, status, last_verified_at
             ) values ($1, $2, $3, $4, $5, $6, $7, 'active', now())",
        )
        .bind(id)
        .bind(name)
        .bind(ciphertext)
        .bind(nonce)
        .bind(key_version)
        .bind(fingerprint)
        .bind(make_default)
        .execute(&mut *transaction)
        .await?;
        transaction.commit().await?;
        self.account(id)
            .await?
            .ok_or_else(|| DatabaseError::InvalidData("new E2B account disappeared".to_owned()))
    }

    pub async fn accounts(&self) -> Result<Vec<E2bAccount>, DatabaseError> {
        sqlx::query(
            "select id, name, credential_fingerprint, is_default, status,
                    last_verified_at, created_at, updated_at
             from e2b_accounts order by is_default desc, lower(name), id",
        )
        .fetch_all(&self.pool)
        .await?
        .into_iter()
        .map(account_from_row)
        .collect()
    }

    pub async fn account(&self, id: Uuid) -> Result<Option<E2bAccount>, DatabaseError> {
        sqlx::query(
            "select id, name, credential_fingerprint, is_default, status,
                    last_verified_at, created_at, updated_at
             from e2b_accounts where id = $1",
        )
        .bind(id)
        .fetch_optional(&self.pool)
        .await?
        .map(account_from_row)
        .transpose()
    }

    pub async fn default_account_id(&self) -> Result<Option<Uuid>, DatabaseError> {
        Ok(
            sqlx::query_scalar(
                "select id from e2b_accounts where is_default and status = 'active'",
            )
            .fetch_optional(&self.pool)
            .await?,
        )
    }

    pub async fn account_credential(
        &self,
        id: Uuid,
        require_active: bool,
    ) -> Result<Option<StoredCredential>, DatabaseError> {
        let row = sqlx::query(
            "select id, credential_ciphertext, credential_nonce, credential_key_version
             from e2b_accounts where id = $1 and (not $2 or status = 'active')",
        )
        .bind(id)
        .bind(require_active)
        .fetch_optional(&self.pool)
        .await?;
        row.map(credential_from_row).transpose()
    }

    pub async fn update_account(
        &self,
        id: Uuid,
        name: Option<&str>,
        is_default: Option<bool>,
        enabled: Option<bool>,
    ) -> Result<Option<E2bAccount>, DatabaseError> {
        let mut transaction = self.pool.begin().await?;
        lock_accounts(&mut transaction).await?;
        if is_default == Some(true) {
            sqlx::query("update e2b_accounts set is_default = false where is_default")
                .execute(&mut *transaction)
                .await?;
        }
        let row = sqlx::query(
            "update e2b_accounts set
                name = coalesce($2, name),
                is_default = case
                    when $3::boolean is null then is_default
                    when $3 then true else false end,
                status = case
                    when $4::boolean is null then status
                    when $4 then 'active' else 'disabled' end
             where id = $1
               and not ($4 = false and is_default and $3 is distinct from false)
             returning id",
        )
        .bind(id)
        .bind(name)
        .bind(is_default)
        .bind(enabled)
        .fetch_optional(&mut *transaction)
        .await?;
        transaction.commit().await?;
        if row.is_none() {
            return Ok(None);
        }
        self.account(id).await
    }

    pub async fn replace_account_credential(
        &self,
        id: Uuid,
        fingerprint: &str,
        ciphertext: &[u8],
        nonce: &[u8],
        key_version: i32,
    ) -> Result<Option<E2bAccount>, DatabaseError> {
        let updated = sqlx::query(
            "update e2b_accounts set credential_fingerprint = $2,
                    credential_ciphertext = $3, credential_nonce = $4,
                    credential_key_version = $5, status = 'active',
                    last_verified_at = now()
             where id = $1 returning id",
        )
        .bind(id)
        .bind(fingerprint)
        .bind(ciphertext)
        .bind(nonce)
        .bind(key_version)
        .fetch_optional(&self.pool)
        .await?;
        if updated.is_none() {
            return Ok(None);
        }
        self.account(id).await
    }

    pub async fn mark_account_verification(
        &self,
        id: Uuid,
        valid: bool,
    ) -> Result<Option<E2bAccount>, DatabaseError> {
        let updated = sqlx::query(
            "update e2b_accounts set
                    status = case when $2 then
                        case when status = 'disabled' then 'disabled' else 'active' end
                        else 'invalid' end,
                    is_default = is_default and $2,
                    last_verified_at = now() where id = $1 returning id",
        )
        .bind(id)
        .bind(valid)
        .fetch_optional(&self.pool)
        .await?;
        if updated.is_none() {
            return Ok(None);
        }
        self.account(id).await
    }

    pub async fn delete_account(&self, id: Uuid) -> Result<bool, DatabaseError> {
        let referenced: bool = sqlx::query_scalar(
            "select exists(select 1 from e2b_hosts where e2b_account_id = $1)
                 or exists(select 1 from e2b_snapshots where e2b_account_id = $1)",
        )
        .bind(id)
        .fetch_one(&self.pool)
        .await?;
        if referenced {
            return Ok(false);
        }
        Ok(sqlx::query("delete from e2b_accounts where id = $1")
            .bind(id)
            .execute(&self.pool)
            .await?
            .rows_affected()
            == 1)
    }

    pub async fn snapshot_source(&self, id: Uuid) -> Result<Option<SnapshotSource>, DatabaseError> {
        let row = sqlx::query(
            "select id, e2b_account_id, e2b_snapshot_id from e2b_snapshots
             where id = $1 and desired_state = 'ready' and observed_state = 'ready'
               and deleted_at is null",
        )
        .bind(id)
        .fetch_optional(&self.pool)
        .await?;
        row.map(|row| {
            Ok(SnapshotSource {
                snapshot_id: row.try_get("id")?,
                e2b_account_id: row.try_get("e2b_account_id")?,
                e2b_snapshot_id: row.try_get("e2b_snapshot_id")?,
            })
        })
        .transpose()
    }

    #[allow(clippy::too_many_arguments)]
    pub async fn create_host(
        &self,
        id: Uuid,
        name: Option<&str>,
        metadata: &Value,
        e2b_account_id: Uuid,
        source_type: &str,
        source_snapshot_id: Option<Uuid>,
        timeout_seconds: u64,
        network_access: bool,
        ram_mb: Option<u32>,
        idempotency_key: &str,
        request_hash: &str,
    ) -> Result<IdempotentResource, DatabaseError> {
        let mut transaction = self.pool.begin().await?;
        let existing = lock_idempotency(
            &mut transaction,
            "host.create",
            idempotency_key,
            request_hash,
        )
        .await?;
        if let Some(existing) = existing {
            transaction.commit().await?;
            return Ok(IdempotentResource {
                id: existing,
                replayed: true,
            });
        }

        sqlx::query(
            "insert into execution_hosts
                (id, kind, name, desired_state, observed_state, metadata, reconcile_after)
             values ($1, 'e2b', $2, 'ready', 'provisioning', $3, now())",
        )
        .bind(id)
        .bind(name)
        .bind(metadata)
        .execute(&mut *transaction)
        .await?;
        sqlx::query(
            "insert into e2b_hosts
                (host_id, e2b_account_id, source_type, source_snapshot_id, timeout_seconds,
                 network_access, ram_mb)
             values ($1, $2, $3, $4, $5, $6, $7)",
        )
        .bind(id)
        .bind(e2b_account_id)
        .bind(source_type)
        .bind(source_snapshot_id)
        .bind(i64::try_from(timeout_seconds).map_err(|_| {
            DatabaseError::InvalidData("host timeout does not fit bigint".to_owned())
        })?)
        .bind(network_access)
        .bind(
            ram_mb.map(i32::try_from).transpose().map_err(|_| {
                DatabaseError::InvalidData("host RAM does not fit integer".to_owned())
            })?,
        )
        .execute(&mut *transaction)
        .await?;
        complete_idempotency(
            &mut transaction,
            "host.create",
            idempotency_key,
            request_hash,
            "host",
            id,
        )
        .await?;
        transaction.commit().await?;
        Ok(IdempotentResource {
            id,
            replayed: false,
        })
    }

    #[allow(clippy::too_many_arguments)]
    pub async fn create_registered_host(
        &self,
        id: Uuid,
        name: Option<&str>,
        metadata: &Value,
        registration_token_hash: &[u8],
        registration_token_expires_at: chrono::DateTime<chrono::Utc>,
        idempotency_key: &str,
        request_hash: &str,
    ) -> Result<IdempotentResource, DatabaseError> {
        let mut transaction = self.pool.begin().await?;
        let existing = lock_idempotency(
            &mut transaction,
            "registered_host.create",
            idempotency_key,
            request_hash,
        )
        .await?;
        if let Some(existing) = existing {
            transaction.commit().await?;
            return Ok(IdempotentResource {
                id: existing,
                replayed: true,
            });
        }
        sqlx::query(
            "insert into execution_hosts
                (id, kind, name, desired_state, observed_state, metadata)
             values ($1, 'registered', $2, 'ready', 'provisioning', $3)",
        )
        .bind(id)
        .bind(name)
        .bind(metadata)
        .execute(&mut *transaction)
        .await?;
        sqlx::query(
            "insert into registered_hosts
                (host_id, registration_token_hash, registration_token_expires_at)
             values ($1, $2, $3)",
        )
        .bind(id)
        .bind(registration_token_hash)
        .bind(registration_token_expires_at)
        .execute(&mut *transaction)
        .await?;
        complete_idempotency(
            &mut transaction,
            "registered_host.create",
            idempotency_key,
            request_hash,
            "host",
            id,
        )
        .await?;
        transaction.commit().await?;
        Ok(IdempotentResource {
            id,
            replayed: false,
        })
    }

    pub async fn host(
        &self,
        id: Uuid,
        include_deleted: bool,
    ) -> Result<Option<ExecutionHost>, DatabaseError> {
        let sql = format!("{HOST_SELECT} where h.id = $1 and ($2 or h.deleted_at is null)");
        sqlx::query(&sql)
            .bind(id)
            .bind(include_deleted)
            .fetch_optional(&self.pool)
            .await?
            .map(host_from_row)
            .transpose()
    }

    pub async fn hosts(
        &self,
        filter: &HostListFilter,
    ) -> Result<Vec<ExecutionHost>, DatabaseError> {
        let sql = format!(
            "{HOST_SELECT}
             where ($1::text is null or h.observed_state = $1)
               and ($2::text is null or h.kind = $2)
               and ($3::uuid is null or e.e2b_account_id = $3)
               and ($4 or h.deleted_at is null)
             order by h.created_at desc, h.id"
        );
        sqlx::query(&sql)
            .bind(filter.state.as_deref())
            .bind(filter.kind.as_deref())
            .bind(filter.e2b_account_id)
            .bind(filter.include_deleted)
            .fetch_all(&self.pool)
            .await?
            .into_iter()
            .map(host_from_row)
            .collect()
    }

    pub async fn update_host(
        &self,
        id: Uuid,
        name: Option<&str>,
        metadata: Option<&Value>,
    ) -> Result<Option<ExecutionHost>, DatabaseError> {
        let updated = sqlx::query(
            "update execution_hosts set name = coalesce($2, name),
                    metadata = coalesce($3, metadata), revision = revision + 1
             where id = $1 and deleted_at is null returning id",
        )
        .bind(id)
        .bind(name)
        .bind(metadata)
        .fetch_optional(&self.pool)
        .await?;
        if updated.is_none() {
            return Ok(None);
        }
        self.host(id, false).await
    }

    pub async fn request_host_state(
        &self,
        id: Uuid,
        desired: &str,
        observed: &str,
    ) -> Result<Option<ExecutionHost>, DatabaseError> {
        let updated = sqlx::query(
            "update execution_hosts set desired_state = $2, observed_state = $3,
                    status_code = null, status_message = null, status_retryable = false,
                    reconcile_after = now(), reconcile_lease_until = null,
                    reconcile_lease_owner = null, revision = revision + 1
             where id = $1 and deleted_at is null returning id",
        )
        .bind(id)
        .bind(desired)
        .bind(observed)
        .fetch_optional(&self.pool)
        .await?;
        if updated.is_none() {
            return Ok(None);
        }
        self.host(id, true).await
    }

    /// Resume-on-use is conditional: never resurrect deleted hosts or interrupt
    /// a pause/snapshot in progress. Concurrent callers share the reconciler.
    pub async fn resume_host_on_use(&self, id: Uuid) -> Result<(), DatabaseError> {
        sqlx::query("update execution_hosts set desired_state='ready', observed_state='resuming', reconcile_after=now(), revision=revision+1 where id=$1 and kind='e2b' and desired_state='paused' and observed_state='paused' and deleted_at is null and not exists(select 1 from e2b_snapshots where source_host_id=$1 and desired_state='ready' and observed_state='creating')")
            .bind(id).execute(&self.pool).await?;
        Ok(())
    }

    pub async fn schedule_host_reconcile(
        &self,
        id: Uuid,
    ) -> Result<Option<ExecutionHost>, DatabaseError> {
        let updated = sqlx::query(
            "update execution_hosts set reconcile_after = now(), reconcile_lease_owner = null,
                    reconcile_lease_until = null, revision = revision + 1
             where id = $1 and deleted_at is null returning id",
        )
        .bind(id)
        .fetch_optional(&self.pool)
        .await?;
        if updated.is_none() {
            return Ok(None);
        }
        self.host(id, false).await
    }

    pub async fn claim_hosts(&self, worker: &str, limit: i64) -> Result<Vec<Uuid>, DatabaseError> {
        Ok(sqlx::query_scalar(
            "with candidates as (
                select id from execution_hosts
                where reconcile_after <= now()
                  and kind = 'e2b'
                  and (reconcile_lease_until is null or reconcile_lease_until < now())
                order by reconcile_after, id
                for update skip locked limit $2
             )
             update execution_hosts h set reconcile_lease_owner = $1,
                    reconcile_lease_until = now() + interval '5 minutes'
             from candidates c where h.id = c.id returning h.id",
        )
        .bind(worker)
        .bind(limit)
        .fetch_all(&self.pool)
        .await?)
    }

    pub async fn e2b_host_work(&self, id: Uuid) -> Result<Option<HostWork>, DatabaseError> {
        let row = sqlx::query(
            "select h.id, h.desired_state, h.observed_state,
                    e.e2b_account_id, e.e2b_sandbox_id, e.source_type,
                    source.e2b_snapshot_id as source_snapshot_provider_id,
                    e.timeout_seconds, e.network_access, e.ram_mb,
                    a.status as account_status, a.credential_ciphertext,
                    a.credential_nonce, a.credential_key_version
             from execution_hosts h
             join e2b_hosts e on e.host_id = h.id
             join e2b_accounts a on a.id = e.e2b_account_id
             left join e2b_snapshots source on source.id = e.source_snapshot_id
             where h.id = $1",
        )
        .bind(id)
        .fetch_optional(&self.pool)
        .await?;
        row.map(|row| {
            let account_id = row.try_get("e2b_account_id")?;
            Ok(HostWork {
                host_id: row.try_get("id")?,
                desired_state: parse_desired_host(row.try_get("desired_state")?)?,
                observed_state: parse_host_state(row.try_get("observed_state")?)?,
                e2b_account_id: account_id,
                account_status: parse_account_status(row.try_get("account_status")?)?,
                e2b_sandbox_id: row.try_get("e2b_sandbox_id")?,
                source_type: row.try_get("source_type")?,
                source_snapshot_provider_id: row.try_get("source_snapshot_provider_id")?,
                timeout_seconds: to_u64(row.try_get("timeout_seconds")?, "timeout_seconds")?,
                network_access: row.try_get("network_access")?,
                ram_mb: row
                    .try_get::<Option<i32>, _>("ram_mb")?
                    .map(|value| {
                        u32::try_from(value).map_err(|_| {
                            DatabaseError::InvalidData("ram_mb must not be negative".to_owned())
                        })
                    })
                    .transpose()?,
                credential: StoredCredential {
                    account_id,
                    ciphertext: row.try_get("credential_ciphertext")?,
                    nonce: row.try_get("credential_nonce")?,
                    key_version: row.try_get("credential_key_version")?,
                },
            })
        })
        .transpose()
    }

    pub async fn renew_registered_host_token(
        &self,
        id: Uuid,
        token_hash: &[u8],
        expires_at: chrono::DateTime<chrono::Utc>,
    ) -> Result<bool, DatabaseError> {
        Ok(sqlx::query(
            "update registered_hosts r set registration_token_hash = $2,
                    registration_token_expires_at = $3, registration_token_used_at = null
             from execution_hosts h
             where r.host_id = $1 and h.id = r.host_id and h.deleted_at is null",
        )
        .bind(id)
        .bind(token_hash)
        .bind(expires_at)
        .execute(&self.pool)
        .await?
        .rows_affected()
            == 1)
    }

    pub async fn claim_registered_host(
        &self,
        token_hash: &[u8],
        installation_id: Uuid,
        daemon_version: &str,
        credential_hash: &[u8],
    ) -> Result<Option<Uuid>, DatabaseError> {
        let row = sqlx::query(
            "update registered_hosts r set installation_id = $2, daemon_version = $3,
                    credential_hash = $4, credential_created_at = now(),
                    credential_revoked_at = null, registered_at = coalesce(registered_at, now()),
                    registration_token_used_at = now(), registration_token_hash = null,
                    registration_token_expires_at = null
             from execution_hosts h
             where r.host_id = h.id and r.registration_token_hash = $1
               and r.registration_token_expires_at > now()
               and r.registration_token_used_at is null
               and h.desired_state = 'ready' and h.deleted_at is null
             returning r.host_id",
        )
        .bind(token_hash)
        .bind(installation_id)
        .bind(daemon_version)
        .bind(credential_hash)
        .fetch_optional(&self.pool)
        .await?;
        Ok(row.map(|row| row.get("host_id")))
    }

    pub async fn authenticate_registered_host(
        &self,
        id: Uuid,
        credential_hash: &[u8],
    ) -> Result<Option<Uuid>, DatabaseError> {
        Ok(sqlx::query_scalar(
            "select r.installation_id from registered_hosts r
             join execution_hosts h on h.id = r.host_id
             where r.host_id = $1 and r.credential_hash = $2
               and r.credential_revoked_at is null and h.desired_state = 'ready'
               and h.deleted_at is null",
        )
        .bind(id)
        .bind(credential_hash)
        .fetch_optional(&self.pool)
        .await?
        .flatten())
    }

    #[allow(clippy::too_many_arguments)]
    pub async fn mark_registered_connected(
        &self,
        id: Uuid,
        connection_id: Uuid,
        daemon_instance_id: Uuid,
        daemon_version: &str,
        protocol_version: u32,
        descriptor: &ExecutionHostDescriptor,
        credential_hash: &[u8],
    ) -> Result<bool, DatabaseError> {
        let mut transaction = self.pool.begin().await?;
        let updated = sqlx::query(
            "update registered_hosts set connection_id = $2, daemon_instance_id = $3,
                    daemon_version = $4, protocol_version = $5, last_connected_at = now()
             where host_id = $1 and credential_hash = $6
               and credential_revoked_at is null returning host_id",
        )
        .bind(id)
        .bind(connection_id)
        .bind(daemon_instance_id)
        .bind(daemon_version)
        .bind(i32::try_from(protocol_version).map_err(|_| {
            DatabaseError::InvalidData("protocol version does not fit integer".to_owned())
        })?)
        .bind(credential_hash)
        .fetch_optional(&mut *transaction)
        .await?;
        if updated.is_none() {
            transaction.rollback().await?;
            return Ok(false);
        }
        sqlx::query(
            "update execution_hosts set observed_state = 'ready', descriptor = $2,
                    supervisor_generation_id = $3, status_code = null, status_message = null,
                    status_retryable = false, last_seen_at = now(), revision = revision + 1
             where id = $1 and kind = 'registered' and desired_state = 'ready'
               and deleted_at is null",
        )
        .bind(id)
        .bind(
            serde_json::to_value(descriptor)
                .map_err(|error| DatabaseError::InvalidData(error.to_string()))?,
        )
        .bind(
            descriptor
                .supervisor_generation_id
                .as_str()
                .parse::<Uuid>()
                .map_err(|error| {
                    DatabaseError::InvalidData(format!("invalid supervisor generation ID: {error}"))
                })?,
        )
        .execute(&mut *transaction)
        .await?;
        transaction.commit().await?;
        Ok(true)
    }

    pub async fn mark_registered_disconnected(
        &self,
        id: Uuid,
        connection_id: Uuid,
    ) -> Result<(), DatabaseError> {
        sqlx::query(
            "with disconnected as (
                update registered_hosts set last_disconnected_at = now()
                where host_id = $1 and connection_id = $2 returning host_id
             )
             update execution_hosts h set observed_state = 'unavailable',
                    status_code = 'HOST_DAEMON_DISCONNECTED',
                    status_message = 'the Host Daemon is not connected',
                    status_retryable = true, revision = revision + 1
             from disconnected d where h.id = d.host_id and h.desired_state = 'ready'
               and h.deleted_at is null",
        )
        .bind(id)
        .bind(connection_id)
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    pub async fn touch_registered_host(
        &self,
        id: Uuid,
        connection_id: Uuid,
    ) -> Result<(), DatabaseError> {
        sqlx::query(
            "update execution_hosts h set last_seen_at = now()
             from registered_hosts r where h.id = $1 and r.host_id = h.id
               and r.connection_id = $2 and h.deleted_at is null",
        )
        .bind(id)
        .bind(connection_id)
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    pub async fn delete_registered_host(&self, id: Uuid) -> Result<bool, DatabaseError> {
        let mut transaction = self.pool.begin().await?;
        let updated = sqlx::query(
            "update registered_hosts r set credential_revoked_at = now(),
                    registration_token_hash = null, registration_token_expires_at = null
             from execution_hosts h where r.host_id = $1 and h.id = r.host_id
               and h.kind = 'registered' and h.deleted_at is null returning r.host_id",
        )
        .bind(id)
        .fetch_optional(&mut *transaction)
        .await?;
        if updated.is_some() {
            sqlx::query(
                "update execution_hosts set desired_state = 'deleted', observed_state = 'deleted',
                        deleted_at = now(), status_code = null, status_message = null,
                        status_retryable = false, descriptor = null,
                        supervisor_generation_id = null, revision = revision + 1
                 where id = $1",
            )
            .bind(id)
            .execute(&mut *transaction)
            .await?;
        }
        transaction.commit().await?;
        Ok(updated.is_some())
    }

    pub async fn mark_registered_hosts_unavailable(&self) -> Result<(), DatabaseError> {
        sqlx::query(
            "update execution_hosts set observed_state = 'unavailable',
                    status_code = 'GATEWAY_RESTARTED',
                    status_message = 'waiting for the Host Daemon to reconnect',
                    status_retryable = true, revision = revision + 1
             where kind = 'registered' and observed_state = 'ready'
               and desired_state = 'ready' and deleted_at is null",
        )
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    pub async fn mark_registered_unavailable(
        &self,
        id: Uuid,
        code: &str,
        message: &str,
    ) -> Result<(), DatabaseError> {
        sqlx::query(
            "update execution_hosts set observed_state = 'unavailable', status_code = $2,
                    status_message = $3, status_retryable = true, revision = revision + 1
             where id = $1 and kind = 'registered' and desired_state = 'ready'
               and deleted_at is null and observed_state <> 'unavailable'",
        )
        .bind(id)
        .bind(code)
        .bind(message)
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    pub async fn attach_provider_host(
        &self,
        id: Uuid,
        sandbox_id: &str,
    ) -> Result<(), DatabaseError> {
        sqlx::query(
            "update e2b_hosts set e2b_sandbox_id = $2, provider_state = 'running',
                    creation_outcome_ambiguous = false, last_provider_check_at = now()
             where host_id = $1 and e2b_sandbox_id is null",
        )
        .bind(id)
        .bind(sandbox_id)
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    pub async fn complete_host_ready(
        &self,
        id: Uuid,
        descriptor: &ExecutionHostDescriptor,
    ) -> Result<(), DatabaseError> {
        sqlx::query(
            "update execution_hosts set observed_state = 'ready', descriptor = $2,
                    supervisor_generation_id = $3, status_code = null, status_message = null,
                    status_retryable = false, last_seen_at = now(), reconcile_after = null,
                    reconcile_attempts = 0, reconcile_lease_owner = null,
                    reconcile_lease_until = null, revision = revision + 1
             where id = $1 and desired_state = 'ready'",
        )
        .bind(id)
        .bind(
            serde_json::to_value(descriptor)
                .map_err(|error| DatabaseError::InvalidData(error.to_string()))?,
        )
        .bind(
            descriptor
                .supervisor_generation_id
                .as_str()
                .parse::<Uuid>()
                .map_err(|error| {
                    DatabaseError::InvalidData(format!("invalid supervisor generation ID: {error}"))
                })?,
        )
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    pub async fn complete_host_paused(&self, id: Uuid) -> Result<(), DatabaseError> {
        sqlx::query(
            "update execution_hosts set observed_state = 'paused', status_code = null,
                    status_message = null, status_retryable = false, reconcile_after = null,
                    reconcile_attempts = 0, reconcile_lease_owner = null,
                    reconcile_lease_until = null, revision = revision + 1
             where id = $1 and desired_state = 'paused'",
        )
        .bind(id)
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    pub async fn complete_host_deleted(&self, id: Uuid) -> Result<(), DatabaseError> {
        sqlx::query(
            "update execution_hosts set observed_state = 'deleted', deleted_at = now(),
                    status_code = null, status_message = null, status_retryable = false,
                    reconcile_after = null, reconcile_attempts = 0,
                    reconcile_lease_owner = null, reconcile_lease_until = null,
                    revision = revision + 1
             where id = $1 and desired_state = 'deleted'",
        )
        .bind(id)
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    pub async fn fail_host(
        &self,
        id: Uuid,
        expected_desired: Option<&DesiredHostState>,
        failure: HostFailure<'_>,
    ) -> Result<(), DatabaseError> {
        let updated = sqlx::query(
            "update execution_hosts set observed_state = $2, status_code = $3,
                    status_message = $4, status_retryable = $5,
                    reconcile_attempts = reconcile_attempts + 1,
                    reconcile_after = case when $5 then
                        now() + make_interval(secs => least(60, (power(2, least(reconcile_attempts, 5)))::integer))
                        else null end,
                    reconcile_lease_owner = null, reconcile_lease_until = null,
                    revision = revision + 1
             where id = $1 and ($6::text is null or desired_state = $6)",
        )
        .bind(id)
        .bind(failure.observed_state)
        .bind(failure.code)
        .bind(failure.message)
        .bind(failure.retryable)
        .bind(expected_desired.map(desired_host_state_name))
        .execute(&self.pool)
        .await?;
        if failure.ambiguous && updated.rows_affected() > 0 {
            sqlx::query(
                "update e2b_hosts set creation_outcome_ambiguous = true where host_id = $1",
            )
            .bind(id)
            .execute(&self.pool)
            .await?;
        }
        Ok(())
    }

    pub async fn release_host(&self, id: Uuid) -> Result<(), DatabaseError> {
        sqlx::query(
            "update execution_hosts set reconcile_lease_owner = null, reconcile_lease_until = null
             where id = $1",
        )
        .bind(id)
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    pub async fn create_snapshot(
        &self,
        id: Uuid,
        host_id: Uuid,
        name: Option<&str>,
        metadata: &Value,
        idempotency_key: &str,
        request_hash: &str,
    ) -> Result<IdempotentResource, DatabaseError> {
        let mut transaction = self.pool.begin().await?;
        let existing = lock_idempotency(
            &mut transaction,
            "snapshot.create",
            idempotency_key,
            request_hash,
        )
        .await?;
        if let Some(existing) = existing {
            transaction.commit().await?;
            return Ok(IdempotentResource {
                id: existing,
                replayed: true,
            });
        }
        // Replay is resolved above, before checking mutable host state. A
        // completed snapshot pauses its source, but its receipt remains valid.
        let host: Option<(String, String, String)> = sqlx::query_as("select kind, desired_state, observed_state from execution_hosts where id=$1 and deleted_at is null for update")
            .bind(host_id).fetch_optional(&mut *transaction).await?;
        let (kind, desired, observed) = host.ok_or(DatabaseError::HostNotFound)?;
        if kind != "e2b" {
            return Err(DatabaseError::SnapshotUnsupported);
        }
        if desired != "ready" || observed != "ready" {
            return Err(DatabaseError::SnapshotHostNotReady);
        }
        let account_id: Uuid = sqlx::query_scalar(
            "select e.e2b_account_id from execution_hosts h
             join e2b_hosts e on e.host_id = h.id
             where h.id = $1 and h.desired_state = 'ready'
               and h.observed_state = 'ready' and h.deleted_at is null",
        )
        .bind(host_id)
        .fetch_one(&mut *transaction)
        .await?;
        sqlx::query(
            "insert into e2b_snapshots
                (id, e2b_account_id, source_host_id, name, metadata, reconcile_after)
             values ($1, $2, $3, $4, $5, now())",
        )
        .bind(id)
        .bind(account_id)
        .bind(host_id)
        .bind(name)
        .bind(metadata)
        .execute(&mut *transaction)
        .await?;
        complete_idempotency(
            &mut transaction,
            "snapshot.create",
            idempotency_key,
            request_hash,
            "snapshot",
            id,
        )
        .await?;
        transaction.commit().await?;
        Ok(IdempotentResource {
            id,
            replayed: false,
        })
    }

    pub async fn snapshot(
        &self,
        id: Uuid,
        include_deleted: bool,
    ) -> Result<Option<E2bSnapshot>, DatabaseError> {
        let sql = format!("{SNAPSHOT_SELECT} where id = $1 and ($2 or deleted_at is null)");
        sqlx::query(&sql)
            .bind(id)
            .bind(include_deleted)
            .fetch_optional(&self.pool)
            .await?
            .map(snapshot_from_row)
            .transpose()
    }

    pub async fn snapshots(
        &self,
        filter: &SnapshotListFilter,
    ) -> Result<Vec<E2bSnapshot>, DatabaseError> {
        let sql = format!(
            "{SNAPSHOT_SELECT}
             where ($1::text is null or observed_state = $1)
               and ($2::uuid is null or e2b_account_id = $2)
               and ($3::uuid is null or source_host_id = $3)
               and ($4 or deleted_at is null)
             order by created_at desc, id"
        );
        sqlx::query(&sql)
            .bind(filter.state.as_deref())
            .bind(filter.e2b_account_id)
            .bind(filter.source_host_id)
            .bind(filter.include_deleted)
            .fetch_all(&self.pool)
            .await?
            .into_iter()
            .map(snapshot_from_row)
            .collect()
    }

    pub async fn update_snapshot(
        &self,
        id: Uuid,
        name: Option<&str>,
        metadata: Option<&Value>,
    ) -> Result<Option<E2bSnapshot>, DatabaseError> {
        let updated = sqlx::query(
            "update e2b_snapshots set name = coalesce($2, name),
                    metadata = coalesce($3, metadata)
             where id = $1 and deleted_at is null returning id",
        )
        .bind(id)
        .bind(name)
        .bind(metadata)
        .fetch_optional(&self.pool)
        .await?;
        if updated.is_none() {
            return Ok(None);
        }
        self.snapshot(id, false).await
    }

    pub async fn request_snapshot_delete(
        &self,
        id: Uuid,
    ) -> Result<Option<E2bSnapshot>, DatabaseError> {
        let updated = sqlx::query(
            "update e2b_snapshots set desired_state = 'deleted', observed_state = 'deleting',
                    status_code = null, status_message = null, reconcile_after = now(),
                    reconcile_lease_owner = null, reconcile_lease_until = null
             where id = $1 and deleted_at is null returning id",
        )
        .bind(id)
        .fetch_optional(&self.pool)
        .await?;
        if updated.is_none() {
            return Ok(None);
        }
        self.snapshot(id, true).await
    }

    pub async fn claim_snapshots(
        &self,
        worker: &str,
        limit: i64,
    ) -> Result<Vec<Uuid>, DatabaseError> {
        Ok(sqlx::query_scalar(
            "with candidates as (
                select id from e2b_snapshots
                where reconcile_after <= now()
                  and (reconcile_lease_until is null or reconcile_lease_until < now())
                order by reconcile_after, id
                for update skip locked limit $2
             )
             update e2b_snapshots s set reconcile_lease_owner = $1,
                    reconcile_lease_until = now() + interval '5 minutes'
             from candidates c where s.id = c.id returning s.id",
        )
        .bind(worker)
        .bind(limit)
        .fetch_all(&self.pool)
        .await?)
    }

    pub async fn snapshot_work(&self, id: Uuid) -> Result<Option<SnapshotWork>, DatabaseError> {
        let row = sqlx::query(
            "select s.id, s.desired_state, s.e2b_snapshot_id, s.source_host_id,
                    e.e2b_sandbox_id as source_sandbox_id, e.timeout_seconds,
                    a.id as account_id, a.status as account_status, a.credential_ciphertext,
                    a.credential_nonce, a.credential_key_version
             from e2b_snapshots s
             join e2b_accounts a on a.id = s.e2b_account_id
             left join e2b_hosts e on e.host_id = s.source_host_id
             where s.id = $1 and a.status = 'active'",
        )
        .bind(id)
        .fetch_optional(&self.pool)
        .await?;
        row.map(|row| {
            let account_id = row.try_get("account_id")?;
            let timeout: Option<i64> = row.try_get("timeout_seconds")?;
            Ok(SnapshotWork {
                id: row.try_get("id")?,
                desired_state: parse_desired_snapshot(row.try_get("desired_state")?)?,
                e2b_snapshot_id: row.try_get("e2b_snapshot_id")?,
                source_host_id: row.try_get("source_host_id")?,
                source_sandbox_id: row.try_get("source_sandbox_id")?,
                timeout_seconds: timeout
                    .map(|value| to_u64(value, "timeout_seconds"))
                    .transpose()?,
                credential: StoredCredential {
                    account_id,
                    ciphertext: row.try_get("credential_ciphertext")?,
                    nonce: row.try_get("credential_nonce")?,
                    key_version: row.try_get("credential_key_version")?,
                },
                account_status: parse_account_status(row.try_get("account_status")?)?,
            })
        })
        .transpose()
    }

    pub async fn complete_snapshot_ready(
        &self,
        id: Uuid,
        provider_id: &str,
    ) -> Result<(), DatabaseError> {
        let mut transaction = self.pool.begin().await?;
        let source_host_id: Option<Uuid> = sqlx::query_scalar(
            "update e2b_snapshots set e2b_snapshot_id = $2, observed_state = 'ready',
                    status_code = null, status_message = null, reconcile_after = null,
                    reconcile_attempts = 0, reconcile_lease_owner = null,
                    reconcile_lease_until = null
             where id = $1 and desired_state = 'ready' returning source_host_id",
        )
        .bind(id)
        .bind(provider_id)
        .fetch_optional(&mut *transaction)
        .await?
        .flatten();
        if let Some(host_id) = source_host_id {
            sqlx::query(
                "update execution_hosts set desired_state = 'paused', observed_state = 'paused',
                        status_code = null, status_message = null, status_retryable = false,
                        reconcile_after = null, revision = revision + 1
                 where id = $1 and deleted_at is null",
            )
            .bind(host_id)
            .execute(&mut *transaction)
            .await?;
        }
        transaction.commit().await?;
        Ok(())
    }

    pub async fn complete_snapshot_deleted(&self, id: Uuid) -> Result<(), DatabaseError> {
        sqlx::query(
            "update e2b_snapshots set observed_state = 'deleted', deleted_at = now(),
                    status_code = null, status_message = null, reconcile_after = null,
                    reconcile_attempts = 0, reconcile_lease_owner = null,
                    reconcile_lease_until = null
             where id = $1 and desired_state = 'deleted'",
        )
        .bind(id)
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    pub async fn fail_snapshot(
        &self,
        id: Uuid,
        code: &str,
        message: &str,
        retryable: bool,
    ) -> Result<(), DatabaseError> {
        sqlx::query(
            "update e2b_snapshots set observed_state = 'failed', status_code = $2,
                    status_message = $3, reconcile_attempts = reconcile_attempts + 1,
                    reconcile_after = case when $4 then
                        now() + make_interval(secs => least(60, (power(2, least(reconcile_attempts, 5)))::integer))
                        else null end,
                    reconcile_lease_owner = null, reconcile_lease_until = null
             where id = $1",
        )
        .bind(id)
        .bind(code)
        .bind(message)
        .bind(retryable)
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    pub async fn release_snapshot(&self, id: Uuid) -> Result<(), DatabaseError> {
        sqlx::query(
            "update e2b_snapshots set reconcile_lease_owner = null, reconcile_lease_until = null
             where id = $1",
        )
        .bind(id)
        .execute(&self.pool)
        .await?;
        Ok(())
    }
}

async fn lock_accounts(transaction: &mut Transaction<'_, Postgres>) -> Result<(), sqlx::Error> {
    sqlx::query("select pg_advisory_xact_lock(hashtext('execution-gateway:e2b-accounts'))")
        .execute(&mut **transaction)
        .await?;
    Ok(())
}

async fn lock_idempotency(
    transaction: &mut Transaction<'_, Postgres>,
    scope: &str,
    key: &str,
    request_hash: &str,
) -> Result<Option<Uuid>, DatabaseError> {
    let lock_key = format!("execution-gateway:{scope}:{key}");
    sqlx::query("select pg_advisory_xact_lock(hashtextextended($1, 0))")
        .bind(lock_key)
        .execute(&mut **transaction)
        .await?;
    let row = sqlx::query(
        "select request_hash, resource_id from idempotency_records
         where scope = $1 and idempotency_key = $2 and expires_at > now()",
    )
    .bind(scope)
    .bind(key)
    .fetch_optional(&mut **transaction)
    .await?;
    let Some(row) = row else {
        sqlx::query(
            "delete from idempotency_records
             where scope = $1 and idempotency_key = $2 and expires_at <= now()",
        )
        .bind(scope)
        .bind(key)
        .execute(&mut **transaction)
        .await?;
        return Ok(None);
    };
    let existing_hash: String = row.try_get("request_hash")?;
    if existing_hash != request_hash {
        return Err(DatabaseError::IdempotencyConflict);
    }
    row.try_get::<Option<Uuid>, _>("resource_id")?
        .ok_or(DatabaseError::InvalidIdempotencyRecord)
        .map(Some)
}

async fn complete_idempotency(
    transaction: &mut Transaction<'_, Postgres>,
    scope: &str,
    key: &str,
    request_hash: &str,
    resource_kind: &str,
    resource_id: Uuid,
) -> Result<(), sqlx::Error> {
    sqlx::query(
        "insert into idempotency_records
            (id, scope, idempotency_key, request_hash, state, resource_kind,
             resource_id, response_status, expires_at)
         values ($1, $2, $3, $4, 'completed', $5, $6, 202, now() + interval '24 hours')",
    )
    .bind(Uuid::now_v7())
    .bind(scope)
    .bind(key)
    .bind(request_hash)
    .bind(resource_kind)
    .bind(resource_id)
    .execute(&mut **transaction)
    .await?;
    Ok(())
}

fn account_from_row(row: sqlx::postgres::PgRow) -> Result<E2bAccount, DatabaseError> {
    Ok(E2bAccount {
        id: row.try_get("id")?,
        name: row.try_get("name")?,
        credential_fingerprint: row.try_get("credential_fingerprint")?,
        is_default: row.try_get("is_default")?,
        status: parse_account_status(row.try_get("status")?)?,
        last_verified_at: row.try_get("last_verified_at")?,
        created_at: row.try_get("created_at")?,
        updated_at: row.try_get("updated_at")?,
    })
}

fn credential_from_row(row: sqlx::postgres::PgRow) -> Result<StoredCredential, DatabaseError> {
    Ok(StoredCredential {
        account_id: row.try_get("id")?,
        ciphertext: row.try_get("credential_ciphertext")?,
        nonce: row.try_get("credential_nonce")?,
        key_version: row.try_get("credential_key_version")?,
    })
}

fn host_from_row(row: sqlx::postgres::PgRow) -> Result<ExecutionHost, DatabaseError> {
    let kind: String = row.try_get("kind")?;
    let descriptor: Option<ExecutionHostDescriptor> = row
        .try_get::<Option<Value>, _>("descriptor")?
        .map(serde_json::from_value)
        .transpose()
        .map_err(|error| DatabaseError::InvalidData(error.to_string()))?;
    let (kind, e2b, registered) = match kind.as_str() {
        "e2b" => {
            let account_id: Uuid = row.try_get("e2b_account_id")?;
            let source_type: String = row.try_get("source_type")?;
            let source = match source_type.as_str() {
                "base" => E2bHostSource::Base {
                    e2b_account_id: Some(account_id),
                    ram: Some(to_u32(row.try_get("ram_mb")?, "ram_mb")?),
                },
                "snapshot" => E2bHostSource::Snapshot {
                    snapshot_id: row.try_get("source_snapshot_id")?,
                },
                value => {
                    return Err(DatabaseError::InvalidData(format!(
                        "unknown host source {value}"
                    )));
                }
            };
            (
                ExecutionHostKind::E2b,
                Some(E2bHostBinding {
                    e2b_account_id: account_id,
                    e2b_sandbox_id: row.try_get("e2b_sandbox_id")?,
                    source,
                    timeout_seconds: to_u64(row.try_get("timeout_seconds")?, "timeout_seconds")?,
                    network_access: row.try_get("network_access")?,
                }),
                None,
            )
        }
        "registered" => (
            ExecutionHostKind::Registered,
            None,
            Some(RegisteredHostBinding {
                installation_id: row.try_get("installation_id")?,
                daemon_version: row.try_get("daemon_version")?,
                protocol_version: row
                    .try_get::<Option<i32>, _>("protocol_version")?
                    .map(|value| {
                        u32::try_from(value).map_err(|_| {
                            DatabaseError::InvalidData(
                                "protocol_version must not be negative".to_owned(),
                            )
                        })
                    })
                    .transpose()?,
                registered_at: row.try_get("registered_at")?,
                last_connected_at: row.try_get("last_connected_at")?,
                last_disconnected_at: row.try_get("last_disconnected_at")?,
            }),
        ),
        value => {
            return Err(DatabaseError::InvalidData(format!(
                "unknown execution host kind {value}"
            )));
        }
    };
    let roots = host_roots(&kind, descriptor.as_ref());
    Ok(ExecutionHost {
        id: row.try_get("id")?,
        kind,
        name: row.try_get("name")?,
        desired_state: parse_desired_host(row.try_get("desired_state")?)?,
        state: parse_host_state(row.try_get("observed_state")?)?,
        status_code: row.try_get("status_code")?,
        status_message: row.try_get("status_message")?,
        status_retryable: row.try_get("status_retryable")?,
        descriptor,
        roots,
        metadata: row.try_get("metadata")?,
        e2b,
        registered,
        last_seen_at: row.try_get("last_seen_at")?,
        revision: row.try_get("revision")?,
        created_at: row.try_get("created_at")?,
        updated_at: row.try_get("updated_at")?,
        deleted_at: row.try_get("deleted_at")?,
    })
}

fn host_roots(
    kind: &ExecutionHostKind,
    descriptor: Option<&ExecutionHostDescriptor>,
) -> Vec<ExecutionRoot> {
    if let Some(descriptor) = descriptor {
        return descriptor.roots.clone();
    }
    match kind {
        ExecutionHostKind::E2b => {
            let config = SupervisorConfig::default();
            vec![ExecutionRoot {
                id: config.root_id,
                name: config.root_name,
                native_path: config.workspace_path,
                read_only: config.read_only,
            }]
        }
        ExecutionHostKind::Registered => Vec::new(),
    }
}

fn snapshot_from_row(row: sqlx::postgres::PgRow) -> Result<E2bSnapshot, DatabaseError> {
    Ok(E2bSnapshot {
        id: row.try_get("id")?,
        e2b_account_id: row.try_get("e2b_account_id")?,
        e2b_snapshot_id: row.try_get("e2b_snapshot_id")?,
        source_host_id: row.try_get("source_host_id")?,
        name: row.try_get("name")?,
        desired_state: parse_desired_snapshot(row.try_get("desired_state")?)?,
        state: parse_snapshot_state(row.try_get("observed_state")?)?,
        status_code: row.try_get("status_code")?,
        status_message: row.try_get("status_message")?,
        metadata: row.try_get("metadata")?,
        created_at: row.try_get("created_at")?,
        updated_at: row.try_get("updated_at")?,
        deleted_at: row.try_get("deleted_at")?,
    })
}

fn parse_account_status(value: String) -> Result<E2bAccountStatus, DatabaseError> {
    match value.as_str() {
        "active" => Ok(E2bAccountStatus::Active),
        "invalid" => Ok(E2bAccountStatus::Invalid),
        "disabled" => Ok(E2bAccountStatus::Disabled),
        _ => Err(DatabaseError::InvalidData(format!(
            "unknown account status {value}"
        ))),
    }
}

fn parse_desired_host(value: String) -> Result<DesiredHostState, DatabaseError> {
    match value.as_str() {
        "ready" => Ok(DesiredHostState::Ready),
        "paused" => Ok(DesiredHostState::Paused),
        "deleted" => Ok(DesiredHostState::Deleted),
        _ => Err(DatabaseError::InvalidData(format!(
            "unknown desired host state {value}"
        ))),
    }
}

fn desired_host_state_name(state: &DesiredHostState) -> &'static str {
    match state {
        DesiredHostState::Ready => "ready",
        DesiredHostState::Paused => "paused",
        DesiredHostState::Deleted => "deleted",
    }
}

fn parse_host_state(value: String) -> Result<ExecutionHostState, DatabaseError> {
    match value.as_str() {
        "provisioning" => Ok(ExecutionHostState::Provisioning),
        "ready" => Ok(ExecutionHostState::Ready),
        "pausing" => Ok(ExecutionHostState::Pausing),
        "paused" => Ok(ExecutionHostState::Paused),
        "resuming" => Ok(ExecutionHostState::Resuming),
        "unavailable" => Ok(ExecutionHostState::Unavailable),
        "deleting" => Ok(ExecutionHostState::Deleting),
        "deleted" => Ok(ExecutionHostState::Deleted),
        "failed" => Ok(ExecutionHostState::Failed),
        "lost" => Ok(ExecutionHostState::Lost),
        _ => Err(DatabaseError::InvalidData(format!(
            "unknown host state {value}"
        ))),
    }
}

fn parse_desired_snapshot(value: String) -> Result<DesiredSnapshotState, DatabaseError> {
    match value.as_str() {
        "ready" => Ok(DesiredSnapshotState::Ready),
        "deleted" => Ok(DesiredSnapshotState::Deleted),
        _ => Err(DatabaseError::InvalidData(format!(
            "unknown desired snapshot state {value}"
        ))),
    }
}

fn parse_snapshot_state(value: String) -> Result<SnapshotState, DatabaseError> {
    match value.as_str() {
        "creating" => Ok(SnapshotState::Creating),
        "ready" => Ok(SnapshotState::Ready),
        "deleting" => Ok(SnapshotState::Deleting),
        "deleted" => Ok(SnapshotState::Deleted),
        "failed" => Ok(SnapshotState::Failed),
        _ => Err(DatabaseError::InvalidData(format!(
            "unknown snapshot state {value}"
        ))),
    }
}

fn to_u64(value: i64, field: &str) -> Result<u64, DatabaseError> {
    u64::try_from(value)
        .map_err(|_| DatabaseError::InvalidData(format!("{field} must not be negative")))
}

fn to_u32(value: i32, field: &str) -> Result<u32, DatabaseError> {
    u32::try_from(value)
        .map_err(|_| DatabaseError::InvalidData(format!("{field} must not be negative")))
}

impl From<DatabaseError> for crate::error::GatewayError {
    fn from(error: DatabaseError) -> Self {
        match error {
            DatabaseError::HostNotFound => crate::error::GatewayError::not_found("execution host"),
            DatabaseError::SnapshotUnsupported => crate::error::GatewayError::conflict(
                "HOST_OPERATION_UNSUPPORTED",
                "Registered Hosts do not support snapshots",
            ),
            DatabaseError::SnapshotHostNotReady => crate::error::GatewayError::conflict(
                "HOST_NOT_READY",
                "only a ready host can be snapshotted",
            ),
            DatabaseError::IdempotencyConflict => {
                crate::error::GatewayError::conflict("IDEMPOTENCY_KEY_REUSED", error.to_string())
            }
            DatabaseError::Sql(ref error) if matches!(error, SqlxError::Database(database) if database.is_unique_violation()) => {
                crate::error::GatewayError::conflict("CONFLICT", "resource already exists")
            }
            error => {
                tracing::error!(%error, "database operation failed");
                crate::error::GatewayError::internal("database operation failed")
            }
        }
    }
}

#[cfg(test)]
mod root_tests {
    use super::*;

    #[test]
    fn provisioning_e2b_host_exposes_writable_workspace_without_descriptor() {
        let roots = host_roots(&ExecutionHostKind::E2b, None);
        assert_eq!(
            serde_json::to_value(roots).unwrap(),
            serde_json::json!([{
                "id": "workspace",
                "name": "Workspace",
                "native_path": "/home/user",
                "read_only": false
            }])
        );
        assert!(host_roots(&ExecutionHostKind::Registered, None).is_empty());
    }
}
