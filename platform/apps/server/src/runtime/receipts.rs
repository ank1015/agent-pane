use super::error::{Result, RuntimeError};
use axum::{
    Json,
    http::StatusCode,
    response::{IntoResponse, Response},
};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use sha2::{Digest, Sha256};
use sqlx::{Postgres, Transaction};
use uuid::Uuid;

#[derive(Clone, Deserialize, Serialize)]
pub(super) struct Reply {
    pub status: u16,
    pub body: Value,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub worker_owner: Option<super::worker_model::Owner>,
}

impl Reply {
    pub fn new(status: u16, body: Value) -> Self {
        Self {
            status,
            body,
            worker_owner: None,
        }
    }
    pub fn owned_by(mut self, owner: super::worker_model::Owner) -> Self {
        self.worker_owner = Some(owner);
        self
    }
}
impl IntoResponse for Reply {
    fn into_response(self) -> Response {
        (
            StatusCode::from_u16(self.status).unwrap_or(StatusCode::INTERNAL_SERVER_ERROR),
            Json(self.body),
        )
            .into_response()
    }
}

pub(super) fn hash(value: &impl Serialize) -> Result<String> {
    // Rebuild objects in key order even if a downstream crate enables preserve_order.
    fn canonical(value: Value) -> Value {
        match value {
            Value::Object(map) => {
                let mut entries: Vec<_> = map.into_iter().collect();
                entries.sort_by(|a, b| a.0.cmp(&b.0));
                Value::Object(
                    entries
                        .into_iter()
                        .map(|(k, v)| (k, canonical(v)))
                        .collect(),
                )
            }
            Value::Array(values) => Value::Array(values.into_iter().map(canonical).collect()),
            value => value,
        }
    }
    let value = serde_json::to_value(value).map_err(|_| RuntimeError::StoredData)?;
    let bytes = serde_json::to_vec(&canonical(value)).map_err(|_| RuntimeError::StoredData)?;
    Ok(format!("{:x}", Sha256::digest(bytes)))
}

pub(super) async fn replay(
    tx: &mut Transaction<'_, Postgres>,
    scope: &str,
    id: Uuid,
    key: &str,
    hash: &str,
) -> Result<Option<Reply>> {
    let lock_key =
        serde_json::to_string(&(scope, id, key)).map_err(|_| RuntimeError::StoredData)?;
    sqlx::query("select pg_advisory_xact_lock(hashtextextended($1,0))")
        .bind(lock_key)
        .execute(&mut **tx)
        .await?;
    let found: Option<(String,Value)> = sqlx::query_as("select request_hash,result from runtime_requests where scope_kind=$1 and scope_id=$2 and key=$3")
        .bind(scope).bind(id).bind(key).fetch_optional(&mut **tx).await?;
    match found {
        Some((previous, result)) if previous == hash => Ok(Some(
            serde_json::from_value(result).map_err(|_| RuntimeError::StoredData)?,
        )),
        Some(_) => Err(RuntimeError::CodedConflict(
            platform_runtime_contracts::ConflictCode::IdempotencyKeyConflict,
            "This idempotency key was already used with a different request.",
        )),
        None => Ok(None),
    }
}

pub(super) async fn save(
    tx: &mut Transaction<'_, Postgres>,
    project: Uuid,
    scope: &str,
    id: Uuid,
    key: &str,
    hash: &str,
    reply: &Reply,
) -> Result<()> {
    sqlx::query("insert into runtime_requests(project_id,scope_kind,scope_id,key,request_hash,result) values ($1,$2,$3,$4,$5,$6)")
        .bind(project).bind(scope).bind(id).bind(key).bind(hash)
        .bind(serde_json::to_value(reply).map_err(|_| RuntimeError::StoredData)?).execute(&mut **tx).await?;
    Ok(())
}

/// The logical operation key survives worker takeover. Only its original
/// authenticated issuer or the run's current owner can retrieve a receipt.
/// This is a read, not permission for an old lease to perform new mutations.
pub(super) async fn worker_replay(
    tx: &mut Transaction<'_, Postgres>,
    scope: &str,
    id: Uuid,
    key: &str,
    hash: &str,
    owner: super::worker_model::Owner,
) -> Result<Option<Reply>> {
    let reply = replay(tx, scope, id, key, hash).await?;
    if let Some(reply) = &reply {
        if reply.worker_owner != Some(owner) {
            super::workers::lock_runs(tx, &[id]).await?;
            super::workers::owned(tx, id, owner).await?;
        }
    }
    Ok(reply)
}
