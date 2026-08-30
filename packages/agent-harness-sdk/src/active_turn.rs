use std::{
    collections::{HashMap, HashSet},
    sync::{Arc, Mutex},
};

use agent_contracts::{
    NewRunMessage, RunCancelled, SessionMessage, SessionMessagesAppended, TurnRequested,
};
use execution_runtime::OperationContext;
use tokio::sync::Mutex as AsyncMutex;
use uuid::Uuid;

use crate::{AgentClient, AgentClientError};

/// A fenced, process-local view of one delivered run turn.
pub struct ActiveTurn {
    agent: AgentClient,
    request: TurnRequested,
    session_revision: AsyncMutex<u64>,
    operation: OperationContext,
}

impl ActiveTurn {
    pub fn new(agent: AgentClient, request: TurnRequested, operation: OperationContext) -> Self {
        Self {
            session_revision: AsyncMutex::new(request.current_session_revision),
            agent,
            request,
            operation,
        }
    }

    #[must_use]
    pub fn request(&self) -> &TurnRequested {
        &self.request
    }

    #[must_use]
    pub fn run_id(&self) -> Uuid {
        self.request.run_id
    }

    #[must_use]
    pub fn turn_number(&self) -> u32 {
        self.request.turn_number
    }

    #[must_use]
    pub fn operation(&self) -> &OperationContext {
        &self.operation
    }

    pub async fn fetch_session_messages(&self) -> Result<Vec<SessionMessage>, ActiveTurnError> {
        Ok(self.agent.fetch_session_messages(self.run_id()).await?)
    }

    pub async fn append_messages(
        &self,
        messages: &[NewRunMessage],
    ) -> Result<SessionMessagesAppended, ActiveTurnError> {
        let mut revision = self.session_revision.lock().await;
        let appended = self
            .agent
            .append_messages(
                self.run_id(),
                self.request.expected_state_version,
                self.request.turn_number,
                *revision,
                messages.to_vec(),
            )
            .await?;
        *revision = appended.current_session_revision;
        Ok(appended)
    }
}

#[derive(Debug, thiserror::Error)]
pub enum ActiveTurnError {
    #[error(transparent)]
    Agent(#[from] AgentClientError),
}

impl ActiveTurnError {
    #[must_use]
    pub fn retryable(&self) -> bool {
        matches!(self, Self::Agent(error) if error.retryable())
    }

    #[must_use]
    pub fn stale(&self) -> bool {
        matches!(self, Self::Agent(error) if error.stale())
    }
}

#[derive(Clone, Default)]
pub(crate) struct ActiveTurns {
    inner: Arc<Mutex<ActiveTurnsState>>,
}

#[derive(Default)]
struct ActiveTurnsState {
    active: HashMap<Uuid, ActiveEntry>,
    cancelled: HashSet<Uuid>,
}

struct ActiveEntry {
    event_id: Uuid,
    state_version: u64,
    operation: OperationContext,
}

impl ActiveTurns {
    pub fn register(
        &self,
        request: &TurnRequested,
        shutdown: &OperationContext,
    ) -> Option<OperationContext> {
        let mut state = self.inner.lock().expect("active turn registry");
        if state.active.contains_key(&request.run_id) {
            return None;
        }
        let operation = shutdown.child();
        state.active.insert(
            request.run_id,
            ActiveEntry {
                event_id: request.event_id,
                state_version: request.expected_state_version,
                operation: operation.clone(),
            },
        );
        Some(operation)
    }

    pub fn unregister(&self, run_id: Uuid, event_id: Uuid) {
        let mut state = self.inner.lock().expect("active turn registry");
        if state
            .active
            .get(&run_id)
            .is_some_and(|entry| entry.event_id == event_id)
        {
            state.active.remove(&run_id);
            state.cancelled.remove(&run_id);
        }
    }

    pub fn cancel(&self, event: &RunCancelled) {
        let mut state = self.inner.lock().expect("active turn registry");
        if let Some(entry) = state.active.get(&event.run_id)
            && event.state_version > entry.state_version
        {
            entry.operation.cancel();
            state.cancelled.insert(event.run_id);
        }
    }

    pub fn was_cancelled(&self, run_id: Uuid) -> bool {
        self.inner
            .lock()
            .expect("active turn registry")
            .cancelled
            .contains(&run_id)
    }
}

#[cfg(test)]
mod tests {
    use agent_contracts::{HARNESS_PROTOCOL_VERSION, RunCancelled, TurnRequested};
    use chrono::Utc;
    use execution_runtime::OperationContext;
    use llm_contracts::JsonObject;
    use uuid::Uuid;

    use super::ActiveTurns;

    #[test]
    fn admits_one_local_delivery_and_only_newer_cancellation_fences_it() {
        let active = ActiveTurns::default();
        let shutdown = OperationContext::new();
        let request = turn_request();
        let operation = active
            .register(&request, &shutdown)
            .expect("first delivery is admitted");
        assert!(active.register(&request, &shutdown).is_none());

        active.cancel(&cancellation(&request, request.expected_state_version));
        assert!(!operation.is_cancelled());
        assert!(!active.was_cancelled(request.run_id));

        active.cancel(&cancellation(&request, request.expected_state_version + 1));
        assert!(operation.is_cancelled());
        assert!(active.was_cancelled(request.run_id));

        active.unregister(request.run_id, request.event_id);
        assert!(!active.was_cancelled(request.run_id));
        assert!(active.register(&request, &shutdown).is_some());
    }

    fn turn_request() -> TurnRequested {
        TurnRequested {
            protocol_version: HARNESS_PROTOCOL_VERSION,
            event_id: Uuid::now_v7(),
            emitted_at: Utc::now(),
            run_id: Uuid::now_v7(),
            session_id: Uuid::now_v7(),
            turn_number: 1,
            max_turns: 10,
            expected_state_version: 4,
            harness_id: "test".to_owned(),
            harness_slug: "test".to_owned(),
            harness_revision_id: "test-v1".to_owned(),
            resolved_config: JsonObject::new(),
            current_session_revision: 0,
            resume: None,
        }
    }

    fn cancellation(request: &TurnRequested, state_version: u64) -> RunCancelled {
        RunCancelled {
            protocol_version: HARNESS_PROTOCOL_VERSION,
            event_id: Uuid::now_v7(),
            emitted_at: Utc::now(),
            run_id: request.run_id,
            turn_number: request.turn_number,
            state_version,
            reason: None,
            payload: JsonObject::new(),
        }
    }
}
