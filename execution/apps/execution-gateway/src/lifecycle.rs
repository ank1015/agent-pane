use std::{str, sync::Arc, time::Duration};

use execution_api::{DesiredHostState, DesiredSnapshotState, E2bAccountStatus};
use execution_core::ExecutionHostId;
use execution_e2b::{E2bError, E2bErrorKind};
use execution_wire::{Operation, RequestEnvelope, RequestId};
use tokio::task::JoinSet;
use uuid::Uuid;

use crate::{
    crypto::CredentialVault,
    database::{Database, HostFailure, HostWork, SnapshotWork, StoredCredential},
    provider::DynE2bProvider,
};

#[derive(Clone)]
pub struct LifecycleReconciler {
    database: Database,
    vault: CredentialVault,
    provider: DynE2bProvider,
    worker_id: Arc<str>,
}

impl LifecycleReconciler {
    pub fn new(database: Database, vault: CredentialVault, provider: DynE2bProvider) -> Self {
        Self {
            database,
            vault,
            provider,
            worker_id: Arc::from(format!("gateway-{}", Uuid::now_v7())),
        }
    }

    pub fn start(self, interval: Duration) {
        tokio::spawn(async move {
            let mut ticker = tokio::time::interval(interval);
            ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
            loop {
                ticker.tick().await;
                if let Err(error) = self.reconcile_once().await {
                    tracing::error!(%error, "lifecycle reconciliation pass failed");
                }
            }
        });
    }

    pub async fn reconcile_once(&self) -> Result<(), crate::database::DatabaseError> {
        let hosts = self.database.claim_hosts(&self.worker_id, 16).await?;
        let snapshots = self.database.claim_snapshots(&self.worker_id, 16).await?;
        let mut tasks = JoinSet::new();
        for id in hosts {
            let reconciler = self.clone();
            tasks.spawn(async move { reconciler.reconcile_host(id).await });
        }
        for id in snapshots {
            let reconciler = self.clone();
            tasks.spawn(async move { reconciler.reconcile_snapshot(id).await });
        }
        while let Some(result) = tasks.join_next().await {
            match result {
                Ok(Ok(())) => {}
                Ok(Err(error)) => tracing::error!(%error, "resource reconciliation failed"),
                Err(error) => tracing::error!(%error, "resource reconciliation task panicked"),
            }
        }
        Ok(())
    }

    async fn reconcile_host(&self, id: Uuid) -> Result<(), crate::database::DatabaseError> {
        let Some(mut work) = self.database.e2b_host_work(id).await? else {
            self.database
                .fail_host(
                    id,
                    None,
                    HostFailure {
                        observed_state: "failed",
                        code: "E2B_ACCOUNT_UNAVAILABLE",
                        message: "the host's E2B account is missing, disabled, or invalid",
                        retryable: false,
                        ambiguous: false,
                    },
                )
                .await?;
            return Ok(());
        };
        if work.account_status != E2bAccountStatus::Active
            && work.desired_state != DesiredHostState::Deleted
        {
            self.database
                .fail_host(
                    id,
                    Some(&work.desired_state),
                    HostFailure {
                        observed_state: "failed",
                        code: "E2B_ACCOUNT_UNAVAILABLE",
                        message: "the host's E2B account is disabled or invalid",
                        retryable: false,
                        ambiguous: false,
                    },
                )
                .await?;
            return Ok(());
        }
        let api_key = match self.decrypt(&work.credential) {
            Ok(value) => value,
            Err(error) => {
                self.database
                    .fail_host(
                        id,
                        Some(&work.desired_state),
                        HostFailure {
                            observed_state: "failed",
                            code: "CREDENTIAL_DECRYPTION_FAILED",
                            message: &error.to_string(),
                            retryable: false,
                            ambiguous: false,
                        },
                    )
                    .await?;
                return Ok(());
            }
        };
        let key = match str::from_utf8(&api_key) {
            Ok(value) => value,
            Err(_) => {
                self.database
                    .fail_host(
                        id,
                        Some(&work.desired_state),
                        HostFailure {
                            observed_state: "failed",
                            code: "INVALID_STORED_CREDENTIAL",
                            message: "the stored E2B credential is not UTF-8",
                            retryable: false,
                            ambiguous: false,
                        },
                    )
                    .await?;
                return Ok(());
            }
        };

        let result = match work.desired_state {
            DesiredHostState::Ready => self.reconcile_host_ready(&mut work, key).await,
            DesiredHostState::Paused => self.reconcile_host_paused(&mut work, key).await,
            DesiredHostState::Deleted => self.reconcile_host_deleted(&work, key).await,
        };
        if let Err(error) = result {
            self.record_host_error(&work, error).await?;
        }
        Ok(())
    }

    async fn reconcile_host_ready(
        &self,
        work: &mut HostWork,
        api_key: &str,
    ) -> Result<(), E2bError> {
        self.ensure_provider_host(work, api_key).await?;
        let sandbox_id = work
            .e2b_sandbox_id
            .as_deref()
            .expect("ensure_provider_host sets the sandbox ID");
        let request = RequestEnvelope::new(RequestId::generate(), Operation::Describe);
        let response = self
            .provider
            .execute(
                api_key,
                sandbox_id,
                ExecutionHostId::new(work.host_id.to_string())
                    .map_err(|error| invalid_configuration(error.to_string()))?,
                work.timeout_seconds,
                request,
            )
            .await?;
        self.database
            .complete_host_ready(work.host_id, &response.descriptor)
            .await
            .map_err(database_provider_error)
    }

    async fn reconcile_host_paused(
        &self,
        work: &mut HostWork,
        api_key: &str,
    ) -> Result<(), E2bError> {
        self.ensure_provider_host(work, api_key).await?;
        self.provider
            .pause_host(
                api_key,
                work.e2b_sandbox_id
                    .as_deref()
                    .expect("ensure_provider_host sets the sandbox ID"),
                work.timeout_seconds,
            )
            .await?;
        self.database
            .complete_host_paused(work.host_id)
            .await
            .map_err(database_provider_error)
    }

    async fn reconcile_host_deleted(&self, work: &HostWork, api_key: &str) -> Result<(), E2bError> {
        if let Some(sandbox_id) = &work.e2b_sandbox_id {
            self.provider
                .delete_host(api_key, sandbox_id, work.timeout_seconds)
                .await?;
        }
        self.database
            .complete_host_deleted(work.host_id)
            .await
            .map_err(database_provider_error)
    }

    async fn ensure_provider_host(
        &self,
        work: &mut HostWork,
        api_key: &str,
    ) -> Result<(), E2bError> {
        if work.e2b_sandbox_id.is_some() {
            return Ok(());
        }
        let sandbox_id = match work.source_type.as_str() {
            "base" => {
                self.provider
                    .create_base(api_key, work.timeout_seconds)
                    .await?
            }
            "snapshot" => {
                let snapshot_id = work.source_snapshot_provider_id.as_deref().ok_or_else(|| {
                    invalid_configuration("the source snapshot has no E2B snapshot ID")
                })?;
                self.provider
                    .create_from_snapshot(api_key, snapshot_id, work.timeout_seconds)
                    .await?
            }
            value => {
                return Err(invalid_configuration(format!(
                    "unsupported stored host source {value}"
                )));
            }
        };
        self.database
            .attach_provider_host(work.host_id, &sandbox_id)
            .await
            .map_err(database_provider_error)?;
        work.e2b_sandbox_id = Some(sandbox_id);
        Ok(())
    }

    async fn record_host_error(
        &self,
        work: &HostWork,
        error: E2bError,
    ) -> Result<(), crate::database::DatabaseError> {
        let state = if error.kind == E2bErrorKind::NotFound && work.e2b_sandbox_id.is_some() {
            "lost"
        } else if error.retryable {
            "unavailable"
        } else {
            "failed"
        };
        let code = match error.kind {
            E2bErrorKind::Authentication => "E2B_AUTHENTICATION_FAILED",
            E2bErrorKind::NotFound => "E2B_HOST_NOT_FOUND",
            E2bErrorKind::RateLimited => "E2B_RATE_LIMITED",
            E2bErrorKind::Unavailable => "E2B_UNAVAILABLE",
            E2bErrorKind::DeadlineExceeded => "E2B_TIMEOUT",
            E2bErrorKind::Protocol => "SUPERVISOR_PROTOCOL_ERROR",
            _ => "E2B_ERROR",
        };
        self.database
            .fail_host(
                work.host_id,
                Some(&work.desired_state),
                HostFailure {
                    observed_state: state,
                    code,
                    message: &error.message,
                    retryable: error.retryable,
                    ambiguous: error.outcome_ambiguous,
                },
            )
            .await
    }

    async fn reconcile_snapshot(&self, id: Uuid) -> Result<(), crate::database::DatabaseError> {
        let Some(work) = self.database.snapshot_work(id).await? else {
            self.database
                .fail_snapshot(
                    id,
                    "E2B_ACCOUNT_UNAVAILABLE",
                    "the snapshot's E2B account is missing, disabled, or invalid",
                    false,
                )
                .await?;
            return Ok(());
        };
        if work.account_status != E2bAccountStatus::Active
            && work.desired_state != DesiredSnapshotState::Deleted
        {
            self.database
                .fail_snapshot(
                    id,
                    "E2B_ACCOUNT_UNAVAILABLE",
                    "the snapshot's E2B account is disabled or invalid",
                    false,
                )
                .await?;
            return Ok(());
        }
        let api_key = match self.decrypt(&work.credential) {
            Ok(value) => value,
            Err(error) => {
                self.database
                    .fail_snapshot(
                        id,
                        "CREDENTIAL_DECRYPTION_FAILED",
                        &error.to_string(),
                        false,
                    )
                    .await?;
                return Ok(());
            }
        };
        let key = match str::from_utf8(&api_key) {
            Ok(value) => value,
            Err(_) => {
                self.database
                    .fail_snapshot(
                        id,
                        "INVALID_STORED_CREDENTIAL",
                        "the stored E2B credential is not UTF-8",
                        false,
                    )
                    .await?;
                return Ok(());
            }
        };
        let result = match work.desired_state {
            DesiredSnapshotState::Ready => self.reconcile_snapshot_ready(&work, key).await,
            DesiredSnapshotState::Deleted => self.reconcile_snapshot_deleted(&work, key).await,
        };
        if let Err(error) = result {
            let code = match error.kind {
                E2bErrorKind::Authentication => "E2B_AUTHENTICATION_FAILED",
                E2bErrorKind::NotFound => "E2B_RESOURCE_NOT_FOUND",
                E2bErrorKind::RateLimited => "E2B_RATE_LIMITED",
                E2bErrorKind::Unavailable => "E2B_UNAVAILABLE",
                E2bErrorKind::DeadlineExceeded => "E2B_TIMEOUT",
                _ => "E2B_ERROR",
            };
            self.database
                .fail_snapshot(id, code, &error.message, error.retryable)
                .await?;
        }
        Ok(())
    }

    async fn reconcile_snapshot_ready(
        &self,
        work: &SnapshotWork,
        api_key: &str,
    ) -> Result<(), E2bError> {
        if let Some(provider_id) = &work.e2b_snapshot_id {
            return self
                .database
                .complete_snapshot_ready(work.id, provider_id)
                .await
                .map_err(database_provider_error);
        }
        let sandbox_id = work
            .source_sandbox_id
            .as_deref()
            .ok_or_else(|| invalid_configuration("the source host has no E2B sandbox ID"))?;
        let timeout = work
            .timeout_seconds
            .ok_or_else(|| invalid_configuration("the source host has no timeout"))?;
        let provider_id = self
            .provider
            .create_snapshot(api_key, sandbox_id, timeout)
            .await?;
        self.database
            .complete_snapshot_ready(work.id, &provider_id)
            .await
            .map_err(database_provider_error)
    }

    async fn reconcile_snapshot_deleted(
        &self,
        work: &SnapshotWork,
        api_key: &str,
    ) -> Result<(), E2bError> {
        if let Some(provider_id) = &work.e2b_snapshot_id {
            self.provider.delete_snapshot(api_key, provider_id).await?;
        }
        self.database
            .complete_snapshot_deleted(work.id)
            .await
            .map_err(database_provider_error)
    }

    fn decrypt(
        &self,
        credential: &StoredCredential,
    ) -> Result<zeroize::Zeroizing<Vec<u8>>, crate::crypto::VaultError> {
        self.vault.decrypt(
            &credential_context(credential.account_id),
            &credential.ciphertext,
            &credential.nonce,
            credential.key_version,
        )
    }
}

pub fn credential_context(account_id: Uuid) -> String {
    format!("execution-gateway:e2b-account:{account_id}")
}

fn invalid_configuration(message: impl Into<String>) -> E2bError {
    E2bError {
        kind: E2bErrorKind::Configuration,
        message: message.into(),
        retryable: false,
        outcome_ambiguous: false,
        status: None,
    }
}

fn database_provider_error(error: crate::database::DatabaseError) -> E2bError {
    invalid_configuration(format!("gateway database update failed: {error}"))
}
