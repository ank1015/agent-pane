use std::time::Duration;

use chrono::{DateTime, Utc};
use execution_contracts::{
    EnvironmentId, ExecutionError, MachineDescriptor, MachineId, TimestampMs, WorkspaceRootId,
};
use execution_protocol::{
    ConnectorKind, Environment, MachineSummary, Operation, OperationEvent, OperationRecord,
    OperationStatus, Response, StreamItem,
};
use sqlx::{PgPool, Row, postgres::PgPoolOptions};
use thiserror::Error;
use uuid::Uuid;

#[derive(Clone)]
pub struct Database {
    pool: PgPool,
}

#[derive(Debug, Error)]
pub enum DbError {
    #[error(transparent)]
    Sql(#[from] sqlx::Error),
    #[error(transparent)]
    Json(#[from] serde_json::Error),
    #[error("stored contract is invalid: {0}")]
    Contract(String),
}

#[derive(Clone)]
pub struct MachineRecord {
    pub summary: MachineSummary,
    pub credential_hash: Option<Vec<u8>>,
}

impl Database {
    pub(crate) fn pool(&self) -> &PgPool {
        &self.pool
    }

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

    pub async fn create_registration(
        &self,
        token_hash: &[u8],
        label: Option<&str>,
        expires_at: DateTime<Utc>,
    ) -> Result<Uuid, DbError> {
        let id = Uuid::now_v7();
        sqlx::query("insert into machine_registrations (id, token_hash, label, expires_at) values ($1, $2, $3, $4)")
            .bind(id).bind(token_hash).bind(label).bind(expires_at)
            .execute(&self.pool).await?;
        Ok(id)
    }

    pub async fn claim_registration(
        &self,
        registration_hash: &[u8],
        credential_hash: &[u8],
        descriptor: &MachineDescriptor,
    ) -> Result<bool, DbError> {
        let mut tx = self.pool.begin().await?;
        let row = sqlx::query("select id from machine_registrations where token_hash = $1 and claimed_at is null and expires_at > now() for update")
            .bind(registration_hash).fetch_optional(&mut *tx).await?;
        let Some(row) = row else {
            return Ok(false);
        };
        let registration_id: Uuid = row.try_get("id")?;
        sqlx::query(
            "insert into machines (machine_id, name, connector_kind, descriptor, credential_hash)
             values ($1, $2, 'machine_daemon', $3, $4)
             on conflict (machine_id) do update set name = excluded.name,
             connector_kind = excluded.connector_kind, descriptor = excluded.descriptor,
             credential_hash = excluded.credential_hash, deleted_at = null, updated_at = now()",
        )
        .bind(descriptor.machine_id.as_str())
        .bind(&descriptor.name)
        .bind(serde_json::to_value(descriptor)?)
        .bind(credential_hash)
        .execute(&mut *tx)
        .await?;
        sqlx::query("update machine_registrations set claimed_at = now() where id = $1")
            .bind(registration_id)
            .execute(&mut *tx)
            .await?;
        tx.commit().await?;
        Ok(true)
    }

    pub async fn machine(&self, machine_id: &str) -> Result<Option<MachineRecord>, DbError> {
        let row =
            sqlx::query("select * from machines where machine_id = $1 and deleted_at is null")
                .bind(machine_id)
                .fetch_optional(&self.pool)
                .await?;
        row.map(machine_from_row).transpose()
    }

    pub async fn machines(&self) -> Result<Vec<MachineRecord>, DbError> {
        sqlx::query(
            "select * from machines where deleted_at is null order by created_at, machine_id",
        )
        .fetch_all(&self.pool)
        .await?
        .into_iter()
        .map(machine_from_row)
        .collect()
    }

    pub async fn mark_seen(
        &self,
        machine_id: &str,
        descriptor: &MachineDescriptor,
    ) -> Result<(), DbError> {
        sqlx::query("update machines set descriptor = $2, last_seen_at = now(), updated_at = now() where machine_id = $1 and deleted_at is null")
            .bind(machine_id).bind(serde_json::to_value(descriptor)?)
            .execute(&self.pool).await?;
        Ok(())
    }

    pub async fn update_machine_name(
        &self,
        machine_id: &str,
        name: &str,
    ) -> Result<Option<MachineRecord>, DbError> {
        let row = sqlx::query(
            "update machines set name = $2, updated_at = now()
             where machine_id = $1 and deleted_at is null returning *",
        )
        .bind(machine_id)
        .bind(name)
        .fetch_optional(&self.pool)
        .await?;
        row.map(machine_from_row).transpose()
    }

    pub async fn delete_machine(&self, machine_id: &str) -> Result<bool, DbError> {
        let mut transaction = self.pool.begin().await?;
        let result = sqlx::query(
            "update machines set credential_hash = null, deleted_at = now(), updated_at = now()
             where machine_id = $1 and deleted_at is null",
        )
        .bind(machine_id)
        .execute(&mut *transaction)
        .await?;
        if result.rows_affected() == 1 {
            sqlx::query(
                "update environments set deleted_at = now()
                 where machine_id = $1 and deleted_at is null",
            )
            .bind(machine_id)
            .execute(&mut *transaction)
            .await?;
        }
        transaction.commit().await?;
        Ok(result.rows_affected() == 1)
    }

    pub async fn create_environment(
        &self,
        environment_id: &EnvironmentId,
        machine_id: &MachineId,
        name: &str,
        workspace_root_id: &WorkspaceRootId,
        path: &str,
    ) -> Result<Option<Environment>, DbError> {
        let row = sqlx::query(
            "insert into environments
                 (environment_id, machine_id, name, workspace_root_id, path)
             select $1, machine_id, $3, $4, $5
             from machines
             where machine_id = $2 and deleted_at is null
             returning *",
        )
        .bind(environment_id.as_str())
        .bind(machine_id.as_str())
        .bind(name)
        .bind(workspace_root_id.as_str())
        .bind(path)
        .fetch_optional(&self.pool)
        .await?;
        row.map(environment_from_row).transpose()
    }

    pub async fn environment(&self, environment_id: &str) -> Result<Option<Environment>, DbError> {
        sqlx::query(
            "select * from environments
             where environment_id = $1 and deleted_at is null",
        )
        .bind(environment_id)
        .fetch_optional(&self.pool)
        .await?
        .map(environment_from_row)
        .transpose()
    }

    pub async fn environments(
        &self,
        machine_id: Option<&str>,
    ) -> Result<Vec<Environment>, DbError> {
        sqlx::query(
            "select * from environments
             where deleted_at is null and ($1::text is null or machine_id = $1)
             order by created_at, environment_id",
        )
        .bind(machine_id)
        .fetch_all(&self.pool)
        .await?
        .into_iter()
        .map(environment_from_row)
        .collect()
    }

    pub async fn update_environment_name(
        &self,
        environment_id: &str,
        name: &str,
    ) -> Result<Option<Environment>, DbError> {
        sqlx::query(
            "update environments set name = $2
             where environment_id = $1 and deleted_at is null
             returning *",
        )
        .bind(environment_id)
        .bind(name)
        .fetch_optional(&self.pool)
        .await?
        .map(environment_from_row)
        .transpose()
    }

    pub async fn delete_environment(&self, environment_id: &str) -> Result<bool, DbError> {
        sqlx::query(
            "update environments set deleted_at = now()
             where environment_id = $1 and deleted_at is null",
        )
        .bind(environment_id)
        .execute(&self.pool)
        .await
        .map(|result| result.rows_affected() == 1)
        .map_err(Into::into)
    }

    pub async fn create_operation(
        &self,
        machine_id: &str,
        environment_id: Option<&EnvironmentId>,
        operation: &Operation,
    ) -> Result<OperationRecord, DbError> {
        let id = Uuid::now_v7();
        let row = sqlx::query("insert into machine_operations (id, machine_id, environment_id, status, operation) values ($1, $2, $3, 'queued', $4) returning *")
            .bind(id).bind(machine_id).bind(environment_id.map(EnvironmentId::as_str))
            .bind(serde_json::to_value(operation)?)
            .fetch_one(&self.pool).await?;
        operation_from_row(row)
    }

    pub async fn operation(&self, id: Uuid) -> Result<Option<OperationRecord>, DbError> {
        sqlx::query("select * from machine_operations where id = $1")
            .bind(id)
            .fetch_optional(&self.pool)
            .await?
            .map(operation_from_row)
            .transpose()
    }

    pub async fn set_running(&self, id: Uuid) -> Result<bool, DbError> {
        sqlx::query(
            "update machine_operations set status = 'running', updated_at = now() where id = $1 and status = 'queued'",
        )
        .bind(id)
        .execute(&self.pool)
        .await
        .map(|result| result.rows_affected() == 1)
        .map_err(Into::into)
    }

    pub async fn complete(&self, id: Uuid, response: Option<&Response>) -> Result<(), DbError> {
        let value = response.map(serde_json::to_value).transpose()?;
        sqlx::query("update machine_operations set status = 'completed', response = $2, updated_at = now() where id = $1 and status = 'running'")
            .bind(id).bind(value).execute(&self.pool).await?;
        Ok(())
    }

    pub async fn fail(&self, id: Uuid, error: &ExecutionError) -> Result<(), DbError> {
        sqlx::query("update machine_operations set status = 'failed', error = $2, updated_at = now() where id = $1 and status in ('queued', 'running')")
            .bind(id).bind(serde_json::to_value(error)?).execute(&self.pool).await?;
        Ok(())
    }

    pub async fn cancel(&self, id: Uuid) -> Result<Option<OperationRecord>, DbError> {
        let row = sqlx::query(
            "update machine_operations set status = 'cancelled', updated_at = now()
             where id = $1 and status in ('queued', 'running') returning *",
        )
        .bind(id)
        .fetch_optional(&self.pool)
        .await?;
        if let Some(row) = row {
            return operation_from_row(row).map(Some);
        }
        self.operation(id).await
    }

    pub async fn add_event(
        &self,
        id: Uuid,
        sequence: u64,
        item: &StreamItem,
    ) -> Result<(), DbError> {
        sqlx::query("insert into machine_operation_events (operation_id, sequence, item) values ($1, $2, $3)")
            .bind(id).bind(i64::try_from(sequence).unwrap_or(i64::MAX))
            .bind(serde_json::to_value(item)?).execute(&self.pool).await?;
        Ok(())
    }

    pub async fn events(
        &self,
        id: Uuid,
        after: u64,
        limit: u32,
    ) -> Result<Vec<OperationEvent>, DbError> {
        let rows = sqlx::query("select sequence, item, created_at from machine_operation_events where operation_id = $1 and sequence > $2 order by sequence limit $3")
            .bind(id).bind(i64::try_from(after).unwrap_or(i64::MAX)).bind(i64::from(limit))
            .fetch_all(&self.pool).await?;
        rows.into_iter()
            .map(|row| {
                let sequence: i64 = row.try_get("sequence")?;
                let item = serde_json::from_value(row.try_get("item")?)?;
                let created: DateTime<Utc> = row.try_get("created_at")?;
                Ok(OperationEvent {
                    sequence: u64::try_from(sequence).unwrap_or(0),
                    item,
                    created_at: timestamp(created),
                })
            })
            .collect()
    }
}

fn machine_from_row(row: sqlx::postgres::PgRow) -> Result<MachineRecord, DbError> {
    let descriptor: MachineDescriptor = serde_json::from_value(row.try_get("descriptor")?)?;
    let connector = connector(row.try_get::<String, _>("connector_kind")?.as_str())?;
    let created: DateTime<Utc> = row.try_get("created_at")?;
    let updated: DateTime<Utc> = row.try_get("updated_at")?;
    let last_seen: Option<DateTime<Utc>> = row.try_get("last_seen_at")?;
    Ok(MachineRecord {
        summary: MachineSummary {
            machine_id: descriptor.machine_id.clone(),
            name: row.try_get("name")?,
            connector,
            online: false,
            descriptor,
            created_at: timestamp(created),
            updated_at: timestamp(updated),
            last_seen_at: last_seen.map(timestamp),
        },
        credential_hash: row.try_get("credential_hash")?,
    })
}

fn environment_from_row(row: sqlx::postgres::PgRow) -> Result<Environment, DbError> {
    let environment_id = EnvironmentId::new(row.try_get::<String, _>("environment_id")?)
        .map_err(|error| DbError::Contract(error.to_string()))?;
    let machine_id = MachineId::new(row.try_get::<String, _>("machine_id")?)
        .map_err(|error| DbError::Contract(error.to_string()))?;
    let workspace_root_id = WorkspaceRootId::new(row.try_get::<String, _>("workspace_root_id")?)
        .map_err(|error| DbError::Contract(error.to_string()))?;
    let created: DateTime<Utc> = row.try_get("created_at")?;
    Ok(Environment {
        environment_id,
        machine_id,
        name: row.try_get("name")?,
        workspace_root_id,
        path: row.try_get("path")?,
        created_at: timestamp(created),
    })
}

fn operation_from_row(row: sqlx::postgres::PgRow) -> Result<OperationRecord, DbError> {
    let id: Uuid = row.try_get("id")?;
    let machine_id = MachineId::new(row.try_get::<String, _>("machine_id")?)
        .map_err(|e| DbError::Contract(e.to_string()))?;
    let created: DateTime<Utc> = row.try_get("created_at")?;
    let updated: DateTime<Utc> = row.try_get("updated_at")?;
    Ok(OperationRecord {
        operation_id: id.to_string(),
        machine_id,
        environment_id: row
            .try_get::<Option<String>, _>("environment_id")?
            .map(EnvironmentId::new)
            .transpose()
            .map_err(|error| DbError::Contract(error.to_string()))?,
        status: status(&row.try_get::<String, _>("status")?)?,
        operation: Box::new(serde_json::from_value(row.try_get("operation")?)?),
        response: row
            .try_get::<Option<serde_json::Value>, _>("response")?
            .map(serde_json::from_value)
            .transpose()?,
        error: row
            .try_get::<Option<serde_json::Value>, _>("error")?
            .map(serde_json::from_value)
            .transpose()?,
        created_at: timestamp(created),
        updated_at: timestamp(updated),
    })
}

fn connector(value: &str) -> Result<ConnectorKind, DbError> {
    match value {
        "machine_daemon" => Ok(ConnectorKind::MachineDaemon),
        "sandbox" => Ok(ConnectorKind::Sandbox),
        "e2b" => Ok(ConnectorKind::E2b),
        "ssh" => Ok(ConnectorKind::Ssh),
        _ => Err(DbError::Contract(format!("unknown connector {value}"))),
    }
}
fn status(value: &str) -> Result<OperationStatus, DbError> {
    match value {
        "queued" => Ok(OperationStatus::Queued),
        "running" => Ok(OperationStatus::Running),
        "completed" => Ok(OperationStatus::Completed),
        "failed" => Ok(OperationStatus::Failed),
        "cancelled" => Ok(OperationStatus::Cancelled),
        _ => Err(DbError::Contract(format!("unknown status {value}"))),
    }
}
fn timestamp(value: DateTime<Utc>) -> TimestampMs {
    TimestampMs(u64::try_from(value.timestamp_millis()).unwrap_or(0))
}
