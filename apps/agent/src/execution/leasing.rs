use base64::{Engine as _, engine::general_purpose::URL_SAFE_NO_PAD};
use llm_contracts::Validate as _;
use sha2::{Digest, Sha256};
use sqlx::{Postgres, Transaction};
use subtle::ConstantTimeEq as _;
use uuid::Uuid;
use zeroize::Zeroizing;

use super::{
    ClaimRun, ClaimedRun, ExecutionError, ExecutionPolicy, HeartbeatRun, Run, RunHeartbeat,
    RunStatus,
    records::{LeaseRow, RUN_COLUMNS, RunRow, WorkerRevisionRow},
};
use crate::{db::Database, harnesses::HarnessRevision};

const LEASE_COLUMNS: &str = "lease_id, run_id, lease_version, worker_instance_id, token_hash, \
    acquired_at, expires_at, expires_at > now() as active";

pub(super) struct LeaseToken(Zeroizing<Vec<u8>>);

impl LeaseToken {
    pub(super) fn parse(value: &str) -> Result<Self, ExecutionError> {
        let bytes = URL_SAFE_NO_PAD
            .decode(value)
            .map_err(|_| ExecutionError::InvalidLeaseToken)?;
        if bytes.len() != 32 {
            return Err(ExecutionError::InvalidLeaseToken);
        }
        Ok(Self(Zeroizing::new(bytes)))
    }

    fn hash(&self) -> [u8; 32] {
        Sha256::digest(self.0.as_slice()).into()
    }
}

pub(super) async fn claim(
    database: &Database,
    policy: ExecutionPolicy,
    command: ClaimRun,
    token: &LeaseToken,
) -> Result<Option<ClaimedRun>, ExecutionError> {
    command.validate()?;
    if command.supported_harness_revision_ids.len() > policy.max_supported_revisions() {
        return Err(ExecutionError::TooManySupportedRevisions(
            policy.max_supported_revisions(),
        ));
    }

    let mut transaction = database.pool().begin().await?;
    if let Some(run_id) =
        sqlx::query_scalar::<_, Uuid>("select run_id from run_leases where lease_id = $1")
            .bind(command.lease_id)
            .fetch_optional(&mut *transaction)
            .await?
    {
        let run = lock_run(&mut transaction, run_id).await?;
        let lease = lock_lease(&mut transaction, run_id).await?;
        verify_lease(&lease, lease_version(&lease)?, token)?;
        if lease.lease_id != command.lease_id
            || lease.worker_instance_id != command.worker_instance_id
            || !command
                .supported_harness_revision_ids
                .contains(&run.harness_revision_id)
        {
            return Err(ExecutionError::LeaseIdConflict(command.lease_id));
        }
        let claimed = claimed_run(&mut transaction, run, lease).await?;
        transaction.commit().await?;
        return Ok(Some(claimed));
    }

    let query = format!(
        "select {RUN_COLUMNS} from runs \
         where status = 'queued' and harness_revision_id = any($1::text[]) \
         order by queued_at, run_id limit 1 for update skip locked"
    );
    let Some(candidate) = sqlx::query_as::<_, RunRow>(&query)
        .bind(&command.supported_harness_revision_ids)
        .fetch_optional(&mut *transaction)
        .await?
    else {
        transaction.rollback().await?;
        return Ok(None);
    };

    let query = format!(
        "update runs set status = 'running', queued_at = null, \
         state_version = state_version + 1, started_at = coalesce(started_at, now()) \
         where run_id = $1 and status = 'queued' returning {RUN_COLUMNS}"
    );
    let run = sqlx::query_as::<_, RunRow>(&query)
        .bind(candidate.run_id)
        .fetch_optional(&mut *transaction)
        .await?
        .ok_or(ExecutionError::LeaseLost)?;
    let version = run.state_version;
    let token_hash = token.hash();
    let query = format!(
        "insert into run_leases \
         (lease_id, run_id, lease_version, worker_instance_id, token_hash, expires_at) \
         values ($1, $2, $3, $4, $5, now() + make_interval(secs => $6)) \
         returning {LEASE_COLUMNS}"
    );
    let lease = sqlx::query_as::<_, LeaseRow>(&query)
        .bind(command.lease_id)
        .bind(run.run_id)
        .bind(version)
        .bind(&command.worker_instance_id)
        .bind(token_hash.as_slice())
        .bind(policy.lease_duration_seconds())
        .fetch_one(&mut *transaction)
        .await
        .map_err(|error| match super::constraint(&error) {
            Some("run_leases_pkey") => ExecutionError::LeaseIdConflict(command.lease_id),
            _ => ExecutionError::Database(error),
        })?;

    let claimed = claimed_run(&mut transaction, run, lease).await?;
    transaction.commit().await?;
    Ok(Some(claimed))
}

pub(super) async fn heartbeat(
    database: &Database,
    policy: ExecutionPolicy,
    run_id: Uuid,
    command: HeartbeatRun,
    token: &LeaseToken,
) -> Result<RunHeartbeat, ExecutionError> {
    command.validate()?;
    let mut transaction = database.pool().begin().await?;
    let run = lock_run(&mut transaction, run_id).await?;
    let status = RunStatus::from_db(&run.status).ok_or_else(|| {
        ExecutionError::InvalidStoredData(format!("unknown run status {:?}", run.status))
    })?;
    if !matches!(status, RunStatus::Running | RunStatus::Aborting) {
        return Err(ExecutionError::RunNotRunnable { run_id, status });
    }
    let lease = lock_lease(&mut transaction, run_id).await?;
    verify_lease(&lease, command.lease_version, token)?;
    let mut directives = Vec::new();
    let expires_at = if status == RunStatus::Aborting {
        let directive = super::aborting::deliver_pending(&mut transaction, run_id).await?;
        let expires_at = sqlx::query_scalar(
            "update run_leases set expires_at = least( \
                 now() + make_interval(secs => $2), $3) \
             where run_id = $1 returning expires_at",
        )
        .bind(run_id)
        .bind(policy.lease_duration_seconds())
        .bind(directive.deadline_at)
        .fetch_one(&mut *transaction)
        .await?;
        directives.push(super::WorkerDirective::Abort(directive));
        expires_at
    } else {
        sqlx::query_scalar(
            "update run_leases set expires_at = now() + make_interval(secs => $2) \
             where run_id = $1 returning expires_at",
        )
        .bind(run_id)
        .bind(policy.lease_duration_seconds())
        .fetch_one(&mut *transaction)
        .await?
    };
    let heartbeat = RunHeartbeat {
        lease_id: lease.lease_id,
        lease_version: command.lease_version,
        state_version: super::records::positive_u64("runs.state_version", run.state_version)?,
        expires_at,
        directives,
    };
    transaction.commit().await?;
    Ok(heartbeat)
}

pub(super) async fn lock_run(
    transaction: &mut Transaction<'_, Postgres>,
    run_id: Uuid,
) -> Result<RunRow, ExecutionError> {
    let query = format!("select {RUN_COLUMNS} from runs where run_id = $1 for update");
    sqlx::query_as::<_, RunRow>(&query)
        .bind(run_id)
        .fetch_optional(&mut **transaction)
        .await?
        .ok_or(ExecutionError::RunNotFound(run_id))
}

pub(super) async fn lock_lease(
    transaction: &mut Transaction<'_, Postgres>,
    run_id: Uuid,
) -> Result<LeaseRow, ExecutionError> {
    let query = format!("select {LEASE_COLUMNS} from run_leases where run_id = $1 for update");
    sqlx::query_as::<_, LeaseRow>(&query)
        .bind(run_id)
        .fetch_optional(&mut **transaction)
        .await?
        .ok_or(ExecutionError::LeaseLost)
}

pub(super) fn verify_lease(
    lease: &LeaseRow,
    expected_version: u64,
    token: &LeaseToken,
) -> Result<(), ExecutionError> {
    if lease_version(lease)? != expected_version || !lease.active {
        return Err(ExecutionError::LeaseLost);
    }
    let actual_hash = token.hash();
    if lease.token_hash.len() != actual_hash.len()
        || !bool::from(lease.token_hash.as_slice().ct_eq(actual_hash.as_slice()))
    {
        return Err(ExecutionError::LeaseLost);
    }
    Ok(())
}

pub(super) fn require_running(row: &RunRow) -> Result<(), ExecutionError> {
    require_status(row, |status| status == RunStatus::Running)
}

pub(super) fn require_running_or_aborting(row: &RunRow) -> Result<(), ExecutionError> {
    require_status(row, |status| {
        matches!(status, RunStatus::Running | RunStatus::Aborting)
    })
}

fn require_status(
    row: &RunRow,
    allowed: impl FnOnce(RunStatus) -> bool,
) -> Result<(), ExecutionError> {
    let status = RunStatus::from_db(&row.status).ok_or_else(|| {
        ExecutionError::InvalidStoredData(format!("unknown run status {:?}", row.status))
    })?;
    if !allowed(status) {
        return Err(ExecutionError::RunNotRunnable {
            run_id: row.run_id,
            status,
        });
    }
    Ok(())
}

fn lease_version(lease: &LeaseRow) -> Result<u64, ExecutionError> {
    super::records::positive_u64("run_leases.lease_version", lease.lease_version)
}

async fn claimed_run(
    transaction: &mut Transaction<'_, Postgres>,
    run: RunRow,
    lease: LeaseRow,
) -> Result<ClaimedRun, ExecutionError> {
    let harness_revision = fetch_revision(transaction, &run.harness_revision_id).await?;
    let current_session_revision =
        sqlx::query_scalar::<_, i64>("select current_revision from sessions where session_id = $1")
            .bind(run.session_id)
            .fetch_one(&mut **transaction)
            .await?;
    let resume = match super::aborting::resume_for_claim(transaction, &run).await? {
        Some(resume) => Some(resume),
        None => super::waiting::resume_for_claim(transaction, &run)
            .await?
            .map(super::RunResume::Wait),
    };
    Ok(ClaimedRun {
        run: Run::try_from(run)?,
        lease: (&lease).try_into()?,
        harness_revision,
        current_session_revision: u64::try_from(current_session_revision).map_err(|error| {
            ExecutionError::InvalidStoredData(format!("sessions.current_revision: {error}"))
        })?,
        resume,
    })
}

async fn fetch_revision(
    transaction: &mut Transaction<'_, Postgres>,
    revision_id: &str,
) -> Result<HarnessRevision, ExecutionError> {
    let row = sqlx::query_as::<_, WorkerRevisionRow>(
        "select r.harness_revision_id, r.harness_id, r.revision, r.contract_version, \
                r.default_config, r.config_schema, r.first_activated_at, r.retired_at, \
                r.created_at, h.active_revision_id \
         from harness_revisions r join harnesses h on h.harness_id = r.harness_id \
         where r.harness_revision_id = $1",
    )
    .bind(revision_id)
    .fetch_optional(&mut **transaction)
    .await?
    .ok_or_else(|| {
        ExecutionError::InvalidStoredData(format!(
            "run references missing harness revision {revision_id:?}"
        ))
    })?;
    row.try_into()
}
