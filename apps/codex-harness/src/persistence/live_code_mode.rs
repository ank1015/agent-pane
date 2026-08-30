use std::{
    collections::HashMap,
    sync::Arc,
    time::{Duration, Instant},
};

use codex_code_mode_runtime::{
    CodeModeSessionDelegate, CodeModeStateStore, InProcessCodeModeSession,
};
use tokio::sync::Mutex;
use uuid::Uuid;

struct LiveSession {
    session: Arc<InProcessCodeModeSession>,
    last_used: Instant,
}

/// Process-local owner of live V8 sessions.
///
/// Serializable cell allocation and `store`/`load` values are delegated to the
/// supplied state store. Removing an entry shuts down all cells owned by it.
#[derive(Clone, Default)]
pub struct LiveCodeModeRegistry {
    sessions: Arc<Mutex<HashMap<Uuid, LiveSession>>>,
}

impl LiveCodeModeRegistry {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    pub async fn get_or_create(
        &self,
        agent_session_id: Uuid,
        delegate: Arc<dyn CodeModeSessionDelegate>,
        state_store: Arc<dyn CodeModeStateStore>,
    ) -> Arc<InProcessCodeModeSession> {
        let mut sessions = self.sessions.lock().await;
        if let Some(entry) = sessions.get_mut(&agent_session_id) {
            entry.last_used = Instant::now();
            return Arc::clone(&entry.session);
        }
        let session = Arc::new(InProcessCodeModeSession::with_delegate_and_state_store(
            delegate,
            state_store,
        ));
        sessions.insert(
            agent_session_id,
            LiveSession {
                session: Arc::clone(&session),
                last_used: Instant::now(),
            },
        );
        session
    }

    pub async fn touch(&self, agent_session_id: Uuid) -> bool {
        let mut sessions = self.sessions.lock().await;
        let Some(entry) = sessions.get_mut(&agent_session_id) else {
            return false;
        };
        entry.last_used = Instant::now();
        true
    }

    /// Shuts down and removes sessions with no harness interaction during the
    /// supplied interval. Live cells in an expired session are terminated.
    pub async fn evict_idle(&self, idle_for: Duration) -> usize {
        let now = Instant::now();
        let expired = {
            let mut sessions = self.sessions.lock().await;
            let ids = sessions
                .iter()
                .filter_map(|(session_id, entry)| {
                    (now.duration_since(entry.last_used) >= idle_for).then_some(*session_id)
                })
                .collect::<Vec<_>>();
            ids.into_iter()
                .filter_map(|session_id| sessions.remove(&session_id))
                .map(|entry| entry.session)
                .collect::<Vec<_>>()
        };
        let count = expired.len();
        for session in expired {
            let _ = session.shutdown().await;
        }
        count
    }

    pub async fn shutdown_session(&self, agent_session_id: Uuid) -> bool {
        let session = self
            .sessions
            .lock()
            .await
            .remove(&agent_session_id)
            .map(|entry| entry.session);
        let Some(session) = session else {
            return false;
        };
        let _ = session.shutdown().await;
        true
    }

    pub async fn shutdown_all(&self) {
        let sessions = {
            let mut sessions = self.sessions.lock().await;
            sessions
                .drain()
                .map(|(_, entry)| entry.session)
                .collect::<Vec<_>>()
        };
        for session in sessions {
            let _ = session.shutdown().await;
        }
    }

    #[must_use]
    pub async fn len(&self) -> usize {
        self.sessions.lock().await.len()
    }

    #[must_use]
    pub async fn is_empty(&self) -> bool {
        self.sessions.lock().await.is_empty()
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;
    use std::time::Duration;

    use codex_code_mode_runtime::{InMemoryCodeModeStateStore, NoopCodeModeSessionDelegate};
    use uuid::Uuid;

    use super::LiveCodeModeRegistry;

    #[tokio::test]
    async fn reuses_sessions_and_releases_expired_entries() {
        let registry = LiveCodeModeRegistry::new();
        let session_id = Uuid::now_v7();
        let first = registry
            .get_or_create(
                session_id,
                Arc::new(NoopCodeModeSessionDelegate),
                Arc::new(InMemoryCodeModeStateStore::default()),
            )
            .await;
        let second = registry
            .get_or_create(
                session_id,
                Arc::new(NoopCodeModeSessionDelegate),
                Arc::new(InMemoryCodeModeStateStore::default()),
            )
            .await;
        assert!(Arc::ptr_eq(&first, &second));
        drop(first);
        drop(second);

        assert_eq!(registry.evict_idle(Duration::ZERO).await, 1);
        assert!(registry.is_empty().await);
    }

    #[tokio::test]
    async fn explicit_shutdown_only_removes_the_selected_session() {
        let registry = LiveCodeModeRegistry::new();
        let first = Uuid::now_v7();
        let second = Uuid::now_v7();
        for session_id in [first, second] {
            registry
                .get_or_create(
                    session_id,
                    Arc::new(NoopCodeModeSessionDelegate),
                    Arc::new(InMemoryCodeModeStateStore::default()),
                )
                .await;
        }

        assert!(registry.shutdown_session(first).await);
        assert_eq!(registry.len().await, 1);
        registry.shutdown_all().await;
        assert!(registry.is_empty().await);
    }
}
