use std::sync::Arc;

use async_trait::async_trait;
use execution_contracts::ExecutionId;
use tokio::sync::{Mutex, MutexGuard};

/// One model-visible Codex process session.
///
/// The execution runtime owns the actual process. Harness state stores this
/// lightweight handle so `write_stdin` can find it and resume after the last
/// event returned to the model.
pub struct CodexExecSession {
    session_id: i32,
    execution_id: ExecutionId,
    tty: bool,
    interaction: Mutex<InteractionState>,
}

#[derive(Debug, Default)]
pub(crate) struct InteractionState {
    pub(crate) last_sequence: u64,
}

impl CodexExecSession {
    /// Creates a session handle. The harness is responsible for choosing a
    /// unique positive model-visible identifier.
    pub fn new(
        session_id: i32,
        execution_id: ExecutionId,
        tty: bool,
    ) -> Result<Self, CodexExecSessionStoreError> {
        Self::with_last_sequence(session_id, execution_id, tty, 0)
    }

    /// Rehydrates a durable session at its last consumed process-event cursor.
    pub fn with_last_sequence(
        session_id: i32,
        execution_id: ExecutionId,
        tty: bool,
        last_sequence: u64,
    ) -> Result<Self, CodexExecSessionStoreError> {
        if session_id <= 0 {
            return Err(CodexExecSessionStoreError::new(
                "session IDs must be positive integers",
            ));
        }
        Ok(Self {
            session_id,
            execution_id,
            tty,
            interaction: Mutex::new(InteractionState { last_sequence }),
        })
    }

    #[must_use]
    pub const fn session_id(&self) -> i32 {
        self.session_id
    }

    #[must_use]
    pub const fn execution_id(&self) -> &ExecutionId {
        &self.execution_id
    }

    #[must_use]
    pub const fn tty(&self) -> bool {
        self.tty
    }

    pub(crate) async fn lock_interaction(&self) -> MutexGuard<'_, InteractionState> {
        self.interaction.lock().await
    }
}

/// Harness-owned storage used to map numeric Codex session IDs to runtime
/// execution IDs. Implementations may use memory, a database, or another
/// task-scoped store.
#[async_trait]
pub trait CodexExecSessionStore: Send + Sync {
    /// Reserves a public session ID for an execution that is about to start.
    /// The tool removes the reservation if process startup fails.
    async fn insert(
        &self,
        execution_id: ExecutionId,
        tty: bool,
    ) -> Result<Arc<CodexExecSession>, CodexExecSessionStoreError>;

    /// Returns an active session by the model-visible numeric ID.
    async fn get(
        &self,
        session_id: i32,
    ) -> Result<Option<Arc<CodexExecSession>>, CodexExecSessionStoreError>;

    /// Persists the last process event consumed by the model-facing session.
    async fn update_last_sequence(
        &self,
        session_id: i32,
        execution_id: &ExecutionId,
        last_sequence: u64,
    ) -> Result<(), CodexExecSessionStoreError>;

    /// Removes the mapping if it still refers to `execution_id`.
    async fn remove(
        &self,
        session_id: i32,
        execution_id: &ExecutionId,
    ) -> Result<(), CodexExecSessionStoreError>;
}

/// Storage-layer failure surfaced by a harness implementation.
#[derive(Clone, Debug, thiserror::Error)]
#[error("{message}")]
pub struct CodexExecSessionStoreError {
    message: String,
}

impl CodexExecSessionStoreError {
    #[must_use]
    pub fn new(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
        }
    }

    #[must_use]
    pub fn message(&self) -> &str {
        &self.message
    }
}
