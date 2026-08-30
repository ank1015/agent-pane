use std::{collections::HashMap, sync::Arc};

use async_trait::async_trait;
use execution_contracts::{ExecutionId, MachineId};
use sqlx::{PgPool, Row};
use tokio::sync::Mutex;
use tool_codex_unified_exec::{
    CodexExecSession, CodexExecSessionStore, CodexExecSessionStoreError,
};
use uuid::Uuid;

type SessionCacheKey = (Uuid, String, i32);
pub(super) type SessionCache = Arc<Mutex<HashMap<SessionCacheKey, Arc<CodexExecSession>>>>;

/// Agent-session-scoped durable mapping used by `exec_command` and
/// `write_stdin`. The process itself remains owned by the execution gateway.
#[derive(Clone)]
pub struct PostgresExecSessionStore {
    pool: PgPool,
    agent_session_id: Uuid,
    machine_id: MachineId,
    cache: SessionCache,
}

impl PostgresExecSessionStore {
    #[must_use]
    pub fn new(pool: PgPool, agent_session_id: Uuid, machine_id: MachineId) -> Self {
        Self::with_cache(
            pool,
            agent_session_id,
            machine_id,
            Arc::new(Mutex::new(HashMap::new())),
        )
    }

    pub(super) const fn with_cache(
        pool: PgPool,
        agent_session_id: Uuid,
        machine_id: MachineId,
        cache: SessionCache,
    ) -> Self {
        Self {
            pool,
            agent_session_id,
            machine_id,
            cache,
        }
    }

    fn cache_key(&self, session_id: i32) -> SessionCacheKey {
        (
            self.agent_session_id,
            self.machine_id.as_str().to_owned(),
            session_id,
        )
    }

    fn error(error: impl std::fmt::Display) -> CodexExecSessionStoreError {
        CodexExecSessionStoreError::new(error.to_string())
    }
}

#[async_trait]
impl CodexExecSessionStore for PostgresExecSessionStore {
    async fn insert(
        &self,
        execution_id: ExecutionId,
        tty: bool,
    ) -> Result<Arc<CodexExecSession>, CodexExecSessionStoreError> {
        let row = sqlx::query(
            "insert into codex_exec_sessions
                (agent_session_id, execution_id, machine_id, tty)
             values ($1, $2, $3, $4)
             returning public_session_id",
        )
        .bind(self.agent_session_id)
        .bind(execution_id.as_str())
        .bind(self.machine_id.as_str())
        .bind(tty)
        .fetch_one(&self.pool)
        .await
        .map_err(Self::error)?;
        let session_id: i32 = row.try_get("public_session_id").map_err(Self::error)?;
        let session = Arc::new(CodexExecSession::new(session_id, execution_id, tty)?);
        self.cache
            .lock()
            .await
            .insert(self.cache_key(session_id), Arc::clone(&session));
        Ok(session)
    }

    async fn get(
        &self,
        session_id: i32,
    ) -> Result<Option<Arc<CodexExecSession>>, CodexExecSessionStoreError> {
        let cache_key = self.cache_key(session_id);
        if let Some(session) = self.cache.lock().await.get(&cache_key).cloned() {
            return Ok(Some(session));
        }
        let row = sqlx::query(
            "update codex_exec_sessions
             set last_accessed_at = now()
             where agent_session_id = $1 and public_session_id = $2 and machine_id = $3
             returning execution_id, tty, last_sequence",
        )
        .bind(self.agent_session_id)
        .bind(session_id)
        .bind(self.machine_id.as_str())
        .fetch_optional(&self.pool)
        .await
        .map_err(Self::error)?;
        let Some(row) = row else {
            return Ok(None);
        };
        let execution_id = ExecutionId::new(
            row.try_get::<String, _>("execution_id")
                .map_err(Self::error)?,
        )
        .map_err(Self::error)?;
        let tty = row.try_get("tty").map_err(Self::error)?;
        let last_sequence = row
            .try_get::<i64, _>("last_sequence")
            .map_err(Self::error)?;
        let last_sequence = u64::try_from(last_sequence).map_err(Self::error)?;
        let loaded = Arc::new(CodexExecSession::with_last_sequence(
            session_id,
            execution_id,
            tty,
            last_sequence,
        )?);
        let mut cache = self.cache.lock().await;
        Ok(Some(
            cache
                .entry(cache_key)
                .or_insert_with(|| Arc::clone(&loaded))
                .clone(),
        ))
    }

    async fn update_last_sequence(
        &self,
        session_id: i32,
        execution_id: &ExecutionId,
        last_sequence: u64,
    ) -> Result<(), CodexExecSessionStoreError> {
        let sequence = i64::try_from(last_sequence).map_err(Self::error)?;
        let updated = sqlx::query(
            "update codex_exec_sessions
             set last_sequence = greatest(last_sequence, $4), last_accessed_at = now()
             where agent_session_id = $1 and public_session_id = $2
               and execution_id = $3 and machine_id = $5",
        )
        .bind(self.agent_session_id)
        .bind(session_id)
        .bind(execution_id.as_str())
        .bind(sequence)
        .bind(self.machine_id.as_str())
        .execute(&self.pool)
        .await
        .map_err(Self::error)?;
        if updated.rows_affected() == 1 {
            Ok(())
        } else {
            Err(CodexExecSessionStoreError::new(
                "unified exec session disappeared while saving its output cursor",
            ))
        }
    }

    async fn remove(
        &self,
        session_id: i32,
        execution_id: &ExecutionId,
    ) -> Result<(), CodexExecSessionStoreError> {
        sqlx::query(
            "delete from codex_exec_sessions
             where agent_session_id = $1 and public_session_id = $2
               and execution_id = $3 and machine_id = $4",
        )
        .bind(self.agent_session_id)
        .bind(session_id)
        .bind(execution_id.as_str())
        .bind(self.machine_id.as_str())
        .execute(&self.pool)
        .await
        .map_err(Self::error)?;
        let mut cache = self.cache.lock().await;
        let cache_key = self.cache_key(session_id);
        if cache
            .get(&cache_key)
            .is_some_and(|session| session.execution_id() == execution_id)
        {
            cache.remove(&cache_key);
        }
        Ok(())
    }
}
