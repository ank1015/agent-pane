//! Session-local private storage. All writes are part of the existing run commit.
use super::{
    RuntimeService,
    error::{Result, RuntimeError},
    mutations,
    worker_model::Owner,
    workers::{lock_runs, owned},
};
use platform_runtime_contracts::{
    ConflictCode, SESSION_STATE_MAX_VALUE_BYTES, SESSION_STATE_MAX_WRITES, SessionStateMutation,
    SessionStateQuery, SessionStateWrite, valid_state_key, valid_state_namespace,
};
use serde_json::{Value, json};
use sqlx::{Postgres, Transaction};
use std::collections::HashSet;
use uuid::Uuid;

pub(super) fn validate(writes: &[SessionStateWrite]) -> Result<()> {
    if writes.len() > SESSION_STATE_MAX_WRITES {
        return Err(RuntimeError::Invalid(
            "Commit at most 200 session-state writes.",
        ));
    }
    let mut keys = HashSet::new();
    for write in writes {
        if !valid_state_namespace(&write.namespace)
            || !valid_state_key(&write.key)
            || write.expected_version < 0
            || write.expected_version == i64::MAX
            || !keys.insert((&write.namespace, &write.key))
        {
            return Err(RuntimeError::Invalid(
                "Session-state writes require valid, unique namespace/key pairs and nonnegative expected versions.",
            ));
        }
        if let SessionStateMutation::Set { value } = &write.mutation {
            if serde_json::to_vec(value)
                .map_err(|_| RuntimeError::StoredData)?
                .len()
                > SESSION_STATE_MAX_VALUE_BYTES
            {
                return Err(RuntimeError::Invalid(
                    "Session-state values must not exceed 256 KiB of JSON.",
                ));
            }
        }
    }
    Ok(())
}

/// Caller holds the session and run locks, checks ownership/versions, and commits
/// these writes with checkpoint, messages, inputs, and disposition in one tx.
pub(super) async fn apply(
    tx: &mut Transaction<'_, Postgres>,
    project: Uuid,
    session: Uuid,
    run: Uuid,
    owner: Owner,
    writes: &[SessionStateWrite],
) -> Result<Vec<Value>> {
    let mut entries = Vec::new();
    for write in writes {
        let current: Option<i64> = sqlx::query_scalar("select version from session_state where session_id=$1 and namespace=$2 and key=$3 for update")
            .bind(session).bind(&write.namespace).bind(&write.key).fetch_optional(&mut **tx).await?;
        if current.unwrap_or(0) != write.expected_version {
            return Err(RuntimeError::CodedConflict(
                ConflictCode::SessionStateVersionConflict,
                "Session state changed. Reload the entry before committing.",
            ));
        }
        let value = match &write.mutation {
            SessionStateMutation::Set { value } => Some(json!(value)),
            SessionStateMutation::Delete {} => None,
        };
        let entry = if current.is_some() {
            sqlx::query_scalar("update session_state set version=version+1,value=$4,saved_by_run_id=$5,saved_by_lease_epoch=$6 where session_id=$1 and namespace=$2 and key=$3 returning to_jsonb(session_state)")
                .bind(session).bind(&write.namespace).bind(&write.key).bind(value).bind(run).bind(owner.lease_epoch)
                .fetch_one(&mut **tx).await?
        } else {
            sqlx::query_scalar("insert into session_state(project_id,session_id,namespace,key,version,value,saved_by_run_id,saved_by_lease_epoch) values($1,$2,$3,$4,1,$5,$6,$7) returning to_jsonb(session_state)")
                .bind(project).bind(session).bind(&write.namespace).bind(&write.key).bind(value).bind(run).bind(owner.lease_epoch)
                .fetch_one(&mut **tx).await?
        };
        entries.push(entry);
    }
    Ok(entries)
}

impl RuntimeService {
    pub(super) async fn session_state(
        &self,
        run: Uuid,
        owner: Owner,
        q: SessionStateQuery,
    ) -> Result<Value> {
        q.validate().map_err(RuntimeError::Invalid)?;
        let count = i64::from(q.limit.unwrap_or(25));
        let mut tx = self.transaction().await?;
        lock_runs(&mut tx, &[run]).await?;
        owned(&mut tx, run, owner).await?;
        let r = mutations::run(&mut tx, run).await?;
        let mut items: Vec<Value> = sqlx::query_scalar("select to_jsonb(s) from session_state s where session_id=$1 and namespace=$2 and ($3::text is null or key=$3) and ($4::text is null or key>$4 collate \"C\") order by key limit $5")
            .bind(r.session_id).bind(q.namespace).bind(q.key).bind(q.after_key).bind(count+1)
            .fetch_all(&mut *tx).await?;
        let next = if items.len() > count as usize {
            items.truncate(count as usize);
            items
                .last()
                .and_then(|item| item["key"].as_str())
                .map(str::to_owned)
        } else {
            None
        };
        owned(&mut tx, run, owner).await?;
        tx.commit().await?;
        Ok(json!({"session_id":r.session_id,"items":items,"next_after_key":next}))
    }
}
