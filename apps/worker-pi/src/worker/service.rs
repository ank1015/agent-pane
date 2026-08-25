use std::{fmt, sync::Arc, time::Duration};

use agent_contracts::{ClaimRun, ClaimedRun, RunStatus};
use base64::{Engine as _, engine::general_purpose::URL_SAFE_NO_PAD};
use execution_runtime::OperationContext;
use rand::{RngCore as _, rngs::OsRng};
use uuid::Uuid;

use crate::{
    clients::{AgentClient, AgentClientError, ClaimResponse},
    config::WorkerConfig,
};

use super::ActiveRun;

const CLAIM_ERROR_RETRY: Duration = Duration::from_millis(250);

#[derive(Clone)]
pub struct WorkerService {
    agent: AgentClient,
    worker_instance_id: Arc<str>,
    supported_harness_revision_ids: Arc<[String]>,
}

impl WorkerService {
    pub fn from_config(config: &WorkerConfig) -> Result<Self, WorkerError> {
        Ok(Self::new(
            AgentClient::new(config.agent.clone())?,
            config.worker_instance_id.clone(),
            config.supported_harness_revision_ids.clone(),
        ))
    }

    #[must_use]
    pub fn new(
        agent: AgentClient,
        worker_instance_id: String,
        supported_harness_revision_ids: Vec<String>,
    ) -> Self {
        Self {
            agent,
            worker_instance_id: worker_instance_id.into(),
            supported_harness_revision_ids: supported_harness_revision_ids.into(),
        }
    }

    /// Polls Agent until a compatible run is claimed or shutdown is requested.
    pub async fn claim_next(&self, shutdown: &OperationContext) -> Result<ActiveRun, WorkerError> {
        let credentials = LeaseCredentials::generate();
        let command = ClaimRun {
            lease_id: credentials.lease_id,
            worker_instance_id: self.worker_instance_id.to_string(),
            supported_harness_revision_ids: self.supported_harness_revision_ids.to_vec(),
        };

        loop {
            let response = tokio::select! {
                () = shutdown.cancelled() => return Err(WorkerError::Cancelled),
                response = self.agent.claim(&command, credentials.token()) => response,
            };
            match response {
                Ok(ClaimResponse::Claimed(claimed)) => {
                    self.validate_claim(&claimed, &credentials)?;
                    return Ok(ActiveRun::new(
                        self.agent.clone(),
                        *claimed,
                        credentials,
                        shutdown,
                    ));
                }
                Ok(ClaimResponse::Empty { retry_after }) => {
                    wait_or_cancel(shutdown, retry_after).await?;
                }
                Err(error) if error.retryable() => {
                    wait_or_cancel(shutdown, CLAIM_ERROR_RETRY).await?;
                }
                Err(error) => return Err(error.into()),
            }
        }
    }

    fn validate_claim(
        &self,
        claimed: &ClaimedRun,
        credentials: &LeaseCredentials,
    ) -> Result<(), WorkerError> {
        let valid = claimed.lease.lease_id == credentials.lease_id
            && claimed.lease.run_id == claimed.run.run_id
            && claimed.lease.worker_instance_id.as_str() == self.worker_instance_id.as_ref()
            && claimed.lease.lease_version > 0
            && claimed.lease.expires_at > claimed.lease.acquired_at
            && claimed.harness_revision.harness_revision_id == claimed.run.harness_revision_id
            && self
                .supported_harness_revision_ids
                .contains(&claimed.run.harness_revision_id)
            && matches!(claimed.run.status, RunStatus::Running | RunStatus::Aborting);
        if valid {
            Ok(())
        } else {
            Err(WorkerError::InvalidClaim)
        }
    }
}

pub(super) struct LeaseCredentials {
    pub lease_id: Uuid,
    token: String,
    lease_version: u64,
}

impl LeaseCredentials {
    fn generate() -> Self {
        let mut bytes = [0_u8; 32];
        OsRng.fill_bytes(&mut bytes);
        Self {
            lease_id: Uuid::now_v7(),
            token: URL_SAFE_NO_PAD.encode(bytes),
            lease_version: 0,
        }
    }

    pub fn token(&self) -> &str {
        &self.token
    }

    pub fn version(&self) -> u64 {
        self.lease_version
    }

    pub fn with_version(mut self, lease_version: u64) -> Self {
        self.lease_version = lease_version;
        self
    }
}

impl fmt::Debug for LeaseCredentials {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("LeaseCredentials")
            .field("lease_id", &self.lease_id)
            .field("token", &"[REDACTED]")
            .finish()
    }
}

#[derive(Debug, thiserror::Error)]
pub enum WorkerError {
    #[error("worker shutdown was requested")]
    Cancelled,
    #[error(transparent)]
    Agent(#[from] AgentClientError),
    #[error("Agent returned a claim that does not match the requested lease")]
    InvalidClaim,
}

async fn wait_or_cancel(
    shutdown: &OperationContext,
    duration: Duration,
) -> Result<(), WorkerError> {
    tokio::select! {
        () = shutdown.cancelled() => Err(WorkerError::Cancelled),
        () = tokio::time::sleep(duration) => Ok(()),
    }
}
