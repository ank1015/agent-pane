use std::sync::{
    Arc, Mutex,
    atomic::{AtomicU64, Ordering},
};

use agent_contracts::{
    AbortDirective, AcknowledgeRunAbort, AppendRunMessages, ClaimedRun, CompleteRunTurn,
    FailRunTurn, NewRunMessage, RequestRunWait, RunAbortAcknowledged, RunMessagesAppended,
    RunTurnCompleted, RunTurnFailed, RunWaitRequested, SessionMessage, TurnCompletionDisposition,
};
use chrono::{DateTime, Utc};
use execution_runtime::OperationContext;
use llm_contracts::JsonObject;
use tokio::{sync::Mutex as AsyncMutex, task::JoinHandle};
use uuid::Uuid;

use crate::clients::{AgentClient, AgentClientError};

use super::{
    heartbeat::{self, HeartbeatState},
    service::LeaseCredentials,
};

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum RunInterruption {
    Abort(AbortDirective),
    LeaseLost,
}

pub struct ActiveRun {
    agent: AgentClient,
    claimed: ClaimedRun,
    lease: Arc<LeaseCredentials>,
    state_version: Arc<AtomicU64>,
    session_revision: AsyncMutex<u64>,
    expires_at: Arc<Mutex<DateTime<Utc>>>,
    interruption: Arc<Mutex<Option<RunInterruption>>>,
    operation: OperationContext,
    heartbeat_stop: OperationContext,
    heartbeat: Option<JoinHandle<()>>,
}

impl ActiveRun {
    pub(super) fn new(
        agent: AgentClient,
        claimed: ClaimedRun,
        credentials: LeaseCredentials,
        shutdown: &OperationContext,
    ) -> Self {
        let lease = Arc::new(credentials.with_version(claimed.lease.lease_version));
        let state_version = Arc::new(AtomicU64::new(claimed.run.state_version));
        let expires_at = Arc::new(Mutex::new(claimed.lease.expires_at));
        let interruption = Arc::new(Mutex::new(None));
        let operation = shutdown.child();
        let heartbeat_stop = OperationContext::new();
        let heartbeat = heartbeat::spawn(
            agent.clone(),
            claimed.run.run_id,
            lease.clone(),
            HeartbeatState {
                state_version: state_version.clone(),
                expires_at: expires_at.clone(),
                interruption: interruption.clone(),
                run_operation: operation.clone(),
            },
            heartbeat_stop.clone(),
            shutdown.clone(),
        );
        Self {
            agent,
            session_revision: AsyncMutex::new(claimed.current_session_revision),
            claimed,
            lease,
            state_version,
            expires_at,
            interruption,
            operation,
            heartbeat_stop,
            heartbeat: Some(heartbeat),
        }
    }

    #[must_use]
    pub fn claimed(&self) -> &ClaimedRun {
        &self.claimed
    }

    #[must_use]
    pub fn run_id(&self) -> Uuid {
        self.claimed.run.run_id
    }

    #[must_use]
    pub fn operation(&self) -> &OperationContext {
        &self.operation
    }

    #[must_use]
    pub fn state_version(&self) -> u64 {
        self.state_version.load(Ordering::Acquire)
    }

    pub async fn session_revision(&self) -> u64 {
        *self.session_revision.lock().await
    }

    #[must_use]
    pub fn expires_at(&self) -> DateTime<Utc> {
        *self.expires_at.lock().expect("heartbeat expiry lock")
    }

    #[must_use]
    pub fn interruption(&self) -> Option<RunInterruption> {
        self.interruption
            .lock()
            .expect("run interruption lock")
            .clone()
    }

    pub async fn fetch_session_messages(&self) -> Result<Vec<SessionMessage>, ActiveRunError> {
        Ok(self
            .agent
            .fetch_session_messages(self.run_id(), self.lease.version(), self.lease.token())
            .await?)
    }

    pub async fn append_messages(
        &self,
        messages: &[NewRunMessage],
    ) -> Result<RunMessagesAppended, ActiveRunError> {
        let mut revision = self.session_revision.lock().await;
        let appended = self
            .agent
            .append_messages(
                self.run_id(),
                &AppendRunMessages {
                    lease_version: self.lease.version(),
                    expected_session_revision: *revision,
                    messages: messages.to_vec(),
                },
                self.lease.token(),
            )
            .await?;
        *revision = appended.current_session_revision;
        Ok(appended)
    }

    pub async fn continue_turn(mut self) -> Result<RunTurnCompleted, ActiveRunError> {
        let result = self
            .agent
            .complete(
                self.run_id(),
                &self.completion(TurnCompletionDisposition::Continue, None),
                self.lease.token(),
            )
            .await;
        self.stop().await;
        result.map_err(Into::into)
    }

    pub async fn complete(
        mut self,
        final_message_id: Uuid,
    ) -> Result<RunTurnCompleted, ActiveRunError> {
        let result = self
            .agent
            .complete(
                self.run_id(),
                &self.completion(TurnCompletionDisposition::Complete, Some(final_message_id)),
                self.lease.token(),
            )
            .await;
        self.stop().await;
        result.map_err(Into::into)
    }

    pub async fn fail(mut self, failure: JsonObject) -> Result<RunTurnFailed, ActiveRunError> {
        let result = self
            .agent
            .fail(
                self.run_id(),
                &FailRunTurn {
                    lease_version: self.lease.version(),
                    expected_state_version: self.state_version(),
                    failure,
                },
                self.lease.token(),
            )
            .await;
        self.stop().await;
        result.map_err(Into::into)
    }

    pub async fn request_wait(
        mut self,
        wait_id: Uuid,
        harness_wait_id: String,
        kind: String,
        public_request: JsonObject,
        resume_metadata: JsonObject,
    ) -> Result<RunWaitRequested, ActiveRunError> {
        let result = self
            .agent
            .request_wait(
                self.run_id(),
                &RequestRunWait {
                    lease_version: self.lease.version(),
                    expected_state_version: self.state_version(),
                    wait_id,
                    harness_wait_id,
                    kind,
                    public_request,
                    resume_metadata,
                },
                self.lease.token(),
            )
            .await;
        self.stop().await;
        result.map_err(Into::into)
    }

    pub async fn acknowledge_abort(
        mut self,
        resume_metadata: JsonObject,
    ) -> Result<RunAbortAcknowledged, ActiveRunError> {
        let abort_id = match self.interruption() {
            Some(RunInterruption::Abort(abort)) => abort.abort_id,
            _ => return Err(ActiveRunError::AbortNotRequested),
        };
        let result = self
            .agent
            .acknowledge_abort(
                self.run_id(),
                &AcknowledgeRunAbort {
                    abort_id,
                    lease_version: self.lease.version(),
                    expected_state_version: self.state_version(),
                    resume_metadata,
                },
                self.lease.token(),
            )
            .await;
        self.stop().await;
        result.map_err(Into::into)
    }

    fn completion(
        &self,
        disposition: TurnCompletionDisposition,
        final_message_id: Option<Uuid>,
    ) -> CompleteRunTurn {
        CompleteRunTurn {
            lease_version: self.lease.version(),
            expected_state_version: self.state_version(),
            disposition,
            final_message_id,
        }
    }

    async fn stop(&mut self) {
        self.heartbeat_stop.cancel();
        self.operation.cancel();
        if let Some(heartbeat) = self.heartbeat.take() {
            let _ = heartbeat.await;
        }
    }
}

impl Drop for ActiveRun {
    fn drop(&mut self) {
        self.heartbeat_stop.cancel();
        self.operation.cancel();
        if let Some(heartbeat) = self.heartbeat.take() {
            heartbeat.abort();
        }
    }
}

#[derive(Debug, thiserror::Error)]
pub enum ActiveRunError {
    #[error(transparent)]
    Agent(#[from] AgentClientError),
    #[error("the run has not received an abort directive")]
    AbortNotRequested,
}

impl ActiveRunError {
    #[must_use]
    pub fn retryable(&self) -> bool {
        matches!(self, Self::Agent(error) if error.retryable())
    }

    #[must_use]
    pub fn lease_is_lost(&self) -> bool {
        matches!(self, Self::Agent(error) if error.lease_is_lost())
    }
}
