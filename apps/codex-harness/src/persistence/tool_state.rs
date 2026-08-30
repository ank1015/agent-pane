use std::{collections::HashMap, future::Future, pin::Pin, sync::Arc, time::Duration};

use codex_code_mode_runtime::{CodeModeSession, CodeModeSessionDelegate, InProcessCodeModeSession};
use execution_contracts::MachineId;
use execution_runtime::OperationContext;
use sqlx::PgPool;
use tokio::sync::Mutex;
use tool_codex_unified_exec::CodexExecSessionStore;
use uuid::Uuid;

use super::{
    LiveCodeModeRegistry, PostgresCodeModeStateStore, PostgresExecSessionStore,
    exec_sessions::SessionCache,
};

/// Harness-level owner for the state needed by Codex's stateful tools.
///
/// Clones share the live code-mode registry and unified-exec handle locks. The
/// actual mappings, output cursors, cell IDs, and `store` values remain in
/// Postgres.
#[derive(Clone)]
pub struct CodexToolState {
    pool: PgPool,
    exec_sessions: SessionCache,
    code_mode_sessions: LiveCodeModeRegistry,
    code_mode_idle_ttl: Duration,
}

pub type CodeModeSessionFuture<'a> =
    Pin<Box<dyn Future<Output = Arc<dyn CodeModeSession>> + Send + 'a>>;

/// Runtime-facing state boundary for Codex's stateful tools.
///
/// The production implementation is Postgres plus the live V8 registry. The
/// interface also keeps tool dispatch independent of that storage choice.
pub trait CodexToolStateBackend: Send + Sync {
    fn exec_session_store(
        &self,
        agent_session_id: Uuid,
        machine_id: MachineId,
    ) -> Arc<dyn CodexExecSessionStore>;

    fn code_mode_session<'a>(
        &'a self,
        agent_session_id: Uuid,
        delegate: Arc<dyn CodeModeSessionDelegate>,
    ) -> CodeModeSessionFuture<'a>;
}

impl CodexToolState {
    #[must_use]
    pub fn new(pool: PgPool, code_mode_idle_ttl: Duration) -> Self {
        Self {
            pool,
            exec_sessions: Arc::new(Mutex::new(HashMap::new())),
            code_mode_sessions: LiveCodeModeRegistry::new(),
            code_mode_idle_ttl: code_mode_idle_ttl.max(Duration::from_millis(1)),
        }
    }

    /// Returns a turn-scoped view over one Agent session's process mappings.
    #[must_use]
    pub fn exec_session_store(
        &self,
        agent_session_id: Uuid,
        machine_id: MachineId,
    ) -> PostgresExecSessionStore {
        PostgresExecSessionStore::with_cache(
            self.pool.clone(),
            agent_session_id,
            machine_id,
            Arc::clone(&self.exec_sessions),
        )
    }

    /// Returns the durable code-mode state for one Agent session.
    #[must_use]
    pub fn code_mode_state_store(&self, agent_session_id: Uuid) -> PostgresCodeModeStateStore {
        PostgresCodeModeStateStore::new(self.pool.clone(), agent_session_id)
    }

    /// Reuses the live V8 owner for a session, or creates it with Postgres-backed
    /// cell allocation and `store`/`load` state.
    pub async fn code_mode_session(
        &self,
        agent_session_id: Uuid,
        delegate: Arc<dyn CodeModeSessionDelegate>,
    ) -> Arc<InProcessCodeModeSession> {
        self.code_mode_sessions
            .get_or_create(
                agent_session_id,
                delegate,
                Arc::new(self.code_mode_state_store(agent_session_id)),
            )
            .await
    }

    /// Releases code-mode sessions idle for the configured interval.
    pub async fn evict_idle_code_mode_sessions(&self) -> usize {
        self.code_mode_sessions
            .evict_idle(self.code_mode_idle_ttl)
            .await
    }

    /// Runs the idle-session collector until harness shutdown.
    pub async fn run_code_mode_reaper(&self, shutdown: &OperationContext) {
        let sweep_interval = self.code_mode_idle_ttl.min(Duration::from_secs(60));
        let mut interval = tokio::time::interval(sweep_interval);
        interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
        loop {
            tokio::select! {
                () = shutdown.cancelled() => return,
                _ = interval.tick() => {
                    let evicted = self.evict_idle_code_mode_sessions().await;
                    if evicted > 0 {
                        tracing::debug!(evicted, "released idle code-mode sessions");
                    }
                }
            }
        }
    }

    /// Releases one Agent session's live V8 cells. Durable state is retained.
    pub async fn shutdown_code_mode_session(&self, agent_session_id: Uuid) -> bool {
        self.code_mode_sessions
            .shutdown_session(agent_session_id)
            .await
    }

    /// Releases every live V8 session during harness shutdown. Durable state is
    /// retained in Postgres.
    pub async fn shutdown(&self) {
        self.code_mode_sessions.shutdown_all().await;
    }
}

impl CodexToolStateBackend for CodexToolState {
    fn exec_session_store(
        &self,
        agent_session_id: Uuid,
        machine_id: MachineId,
    ) -> Arc<dyn CodexExecSessionStore> {
        Arc::new(CodexToolState::exec_session_store(
            self,
            agent_session_id,
            machine_id,
        ))
    }

    fn code_mode_session<'a>(
        &'a self,
        agent_session_id: Uuid,
        delegate: Arc<dyn CodeModeSessionDelegate>,
    ) -> CodeModeSessionFuture<'a> {
        Box::pin(async move {
            let session = CodexToolState::code_mode_session(self, agent_session_id, delegate).await;
            session as Arc<dyn CodeModeSession>
        })
    }
}
