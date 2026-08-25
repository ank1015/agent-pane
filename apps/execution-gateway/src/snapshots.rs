use chrono::{DateTime, Utc};
use execution_contracts::TimestampMs;
use serde::{Deserialize, Serialize};
use sqlx::Row;
use uuid::Uuid;

use crate::{
    db::{Database, DbError},
    sandbox_accounts::SandboxProvider,
};

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct Snapshot {
    pub id: Uuid,
    pub sandbox_account_id: Uuid,
    pub provider: SandboxProvider,
    pub provider_snapshot_id: String,
    pub sandbox_id: String,
    pub created_at: TimestampMs,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct CreateSnapshotRequest {
    pub provider: SandboxProvider,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sandbox_account_id: Option<Uuid>,
    #[serde(alias = "snapshot_sandbox_id")]
    pub provider_snapshot_id: String,
    pub sandbox_id: String,
}

impl Database {
    pub async fn create_snapshot(
        &self,
        id: Uuid,
        sandbox_account_id: Uuid,
        request: &CreateSnapshotRequest,
    ) -> Result<Snapshot, DbError> {
        sqlx::query(
            "insert into snapshots (id, sandbox_account_id, provider_snapshot_id, sandbox_id)
             values ($1, $2, $3, $4)",
        )
        .bind(id)
        .bind(sandbox_account_id)
        .bind(&request.provider_snapshot_id)
        .bind(&request.sandbox_id)
        .execute(self.pool())
        .await?;
        self.snapshot(id)
            .await?
            .ok_or_else(|| DbError::Contract("created snapshot disappeared".to_owned()))
    }

    pub async fn snapshot(&self, id: Uuid) -> Result<Option<Snapshot>, DbError> {
        sqlx::query(&format!("{SNAPSHOT_SELECT} where s.id = $1"))
            .bind(id)
            .fetch_optional(self.pool())
            .await?
            .map(snapshot_from_row)
            .transpose()
    }

    pub async fn snapshots(
        &self,
        provider: Option<SandboxProvider>,
        sandbox_account_id: Option<Uuid>,
        sandbox_id: Option<&str>,
    ) -> Result<Vec<Snapshot>, DbError> {
        sqlx::query(&format!(
            "{SNAPSHOT_SELECT}
                 where ($1::text is null or a.provider = $1)
                   and ($2::uuid is null or s.sandbox_account_id = $2)
                   and ($3::text is null or s.sandbox_id = $3)
                 order by s.created_at desc, s.id"
        ))
        .bind(provider.map(SandboxProvider::as_str))
        .bind(sandbox_account_id)
        .bind(sandbox_id)
        .fetch_all(self.pool())
        .await?
        .into_iter()
        .map(snapshot_from_row)
        .collect()
    }

    pub async fn delete_snapshot(&self, id: Uuid) -> Result<bool, DbError> {
        sqlx::query("delete from snapshots where id = $1")
            .bind(id)
            .execute(self.pool())
            .await
            .map(|result| result.rows_affected() == 1)
            .map_err(Into::into)
    }
}

const SNAPSHOT_SELECT: &str =
    "select s.id, s.sandbox_account_id, a.provider, s.provider_snapshot_id,
            s.sandbox_id, s.created_at
     from snapshots s
     join sandbox_accounts a on a.id = s.sandbox_account_id";

pub(crate) fn validate_request(request: &CreateSnapshotRequest) -> Result<(), &'static str> {
    validate_external_id(&request.provider_snapshot_id, "provider_snapshot_id")?;
    validate_external_id(&request.sandbox_id, "sandbox_id")
}

fn validate_external_id(value: &str, field: &'static str) -> Result<(), &'static str> {
    if value.is_empty() || value != value.trim() {
        return Err(field);
    }
    Ok(())
}

fn snapshot_from_row(row: sqlx::postgres::PgRow) -> Result<Snapshot, DbError> {
    let provider = row
        .try_get::<String, _>("provider")?
        .parse::<SandboxProvider>()
        .map_err(|error| DbError::Contract(error.to_string()))?;
    let created_at: DateTime<Utc> = row.try_get("created_at")?;
    Ok(Snapshot {
        id: row.try_get("id")?,
        sandbox_account_id: row.try_get("sandbox_account_id")?,
        provider,
        provider_snapshot_id: row.try_get("provider_snapshot_id")?,
        sandbox_id: row.try_get("sandbox_id")?,
        created_at: TimestampMs(u64::try_from(created_at.timestamp_millis()).unwrap_or(0)),
    })
}

#[cfg(test)]
mod tests {
    use super::{CreateSnapshotRequest, validate_request};
    use crate::sandbox_accounts::SandboxProvider;

    #[test]
    fn validates_external_snapshot_identifiers() {
        let mut request = CreateSnapshotRequest {
            provider: SandboxProvider::E2b,
            sandbox_account_id: None,
            provider_snapshot_id: "snapshot-1".to_owned(),
            sandbox_id: "sandbox-1".to_owned(),
        };
        assert!(validate_request(&request).is_ok());

        request.provider_snapshot_id = " snapshot-1".to_owned();
        assert_eq!(validate_request(&request), Err("provider_snapshot_id"));
        request.provider_snapshot_id = "snapshot-1".to_owned();
        request.sandbox_id.clear();
        assert_eq!(validate_request(&request), Err("sandbox_id"));
    }
}
