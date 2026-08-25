use chrono::Utc;
use llm_contracts::{JsonObject, Validate as _};
use sqlx::{Postgres, QueryBuilder, Transaction, types::Json};
use uuid::Uuid;

use super::{
    AbortDirective, AbortFinalizationReason, AbortListQuery, AcknowledgeRunAbort, ExecutionError,
    ExecutionPolicy, RequestRunAbort, ResumeRun, Run, RunAbort, RunAbortAcknowledged, RunAbortPage,
    RunAbortResult, RunResume, RunResumed, RunStatus,
    abort_records::{ABORT_COLUMNS, AbortRow},
    finishing::{delete_lease, discard_pending_messages, require_state_version},
    leasing::{LeaseToken, lock_lease, lock_run, verify_lease},
    records::{LeaseRow, RUN_COLUMNS, RunRow},
    waiting,
};
use crate::db::Database;

const DEFAULT_PAGE_SIZE: u32 = 50;
const MAX_PAGE_SIZE: u32 = 100;

pub(super) struct AbortOutcome<T> {
    pub value: T,
    pub created: bool,
}

pub(super) async fn request(
    database: &Database,
    policy: ExecutionPolicy,
    run_id: Uuid,
    command: RequestRunAbort,
) -> Result<AbortOutcome<RunAbortResult>, ExecutionError> {
    command.validate()?;
    let mut transaction = database.pool().begin().await?;
    if let Some(existing) = find_abort(&mut transaction, command.abort_id).await? {
        if existing.run_id != run_id
            || existing.reason != command.reason
            || existing.payload.0 != command.payload
        {
            return Err(ExecutionError::AbortIdConflict(command.abort_id));
        }
        let run = fetch_run(&mut transaction, run_id).await?;
        let value = RunAbortResult {
            abort: existing.try_into()?,
            run,
        };
        transaction.commit().await?;
        return Ok(AbortOutcome {
            value,
            created: false,
        });
    }

    let run = lock_run(&mut transaction, run_id).await?;
    require_state_version(&run, command.expected_state_version)?;
    let status = run_status(&run)?;
    if status == RunStatus::Aborting {
        return Err(ExecutionError::AbortInProgress(run_id));
    }
    if !matches!(
        status,
        RunStatus::Queued | RunStatus::Running | RunStatus::Waiting
    ) {
        return Err(ExecutionError::RunNotAbortable { run_id, status });
    }
    discard_pending_messages(&mut transaction, run_id, "run_aborted").await?;
    let sequence = next_sequence(&mut transaction, run_id).await?;
    let grace_seconds = policy.abort_grace_seconds();

    let (pending, finalization_reason, resume_metadata) = match status {
        RunStatus::Running => {
            let lease = lock_lease(&mut transaction, run_id).await?;
            if lease.active {
                (true, None, JsonObject::new())
            } else {
                delete_lease(&mut transaction, run_id).await?;
                (
                    false,
                    Some(AbortFinalizationReason::LeaseExpired),
                    JsonObject::new(),
                )
            }
        }
        RunStatus::Waiting => (
            false,
            Some(AbortFinalizationReason::NoWorker),
            waiting::cancel_pending(&mut transaction, run_id)
                .await?
                .unwrap_or_default(),
        ),
        RunStatus::Queued => (
            false,
            Some(AbortFinalizationReason::NoWorker),
            JsonObject::new(),
        ),
        _ => unreachable!("abortable status checked above"),
    };

    let query = if pending {
        format!(
            "insert into run_aborts \
             (abort_id, run_id, turn_number, sequence, reason, payload, deadline_at) \
             values ($1, $2, $3, $4, $5, $6, now() + make_interval(secs => $7)) \
             returning {ABORT_COLUMNS}"
        )
    } else {
        format!(
            "insert into run_aborts \
             (abort_id, run_id, turn_number, sequence, reason, payload, status, deadline_at, \
              finalized_at, finalization_reason, resume_metadata) \
             values ($1, $2, $3, $4, $5, $6, 'finalized', \
                     now() + make_interval(secs => $7), now(), $8, $9) \
             returning {ABORT_COLUMNS}"
        )
    };
    let mut insert = sqlx::query_as::<_, AbortRow>(&query)
        .bind(command.abort_id)
        .bind(run_id)
        .bind(run.current_turn)
        .bind(sequence)
        .bind(&command.reason)
        .bind(Json(command.payload))
        .bind(grace_seconds);
    if !pending {
        insert = insert
            .bind(
                finalization_reason
                    .expect("immediate abort has a reason")
                    .as_db(),
            )
            .bind(Json(resume_metadata));
    }
    let abort =
        insert.fetch_one(&mut *transaction).await.map_err(|error| {
            match super::constraint(&error) {
                Some("run_aborts_pkey") => ExecutionError::AbortIdConflict(command.abort_id),
                Some("run_aborts_one_pending_per_run_idx") => {
                    ExecutionError::AbortInProgress(run_id)
                }
                _ => ExecutionError::Database(error),
            }
        })?;

    if pending {
        sqlx::query("update run_leases set expires_at = least(expires_at, $2) where run_id = $1")
            .bind(run_id)
            .bind(abort.deadline_at)
            .execute(&mut *transaction)
            .await?;
    }
    let run = update_aborted_state(&mut transaction, run_id, pending).await?;
    let value = RunAbortResult {
        abort: abort.try_into()?,
        run: run.try_into()?,
    };
    transaction.commit().await?;
    Ok(AbortOutcome {
        value,
        created: true,
    })
}

pub(super) async fn acknowledge(
    database: &Database,
    run_id: Uuid,
    command: AcknowledgeRunAbort,
    token: &LeaseToken,
) -> Result<AbortOutcome<RunAbortAcknowledged>, ExecutionError> {
    command.validate()?;
    let mut transaction = database.pool().begin().await?;
    if let Some(existing) = find_abort(&mut transaction, command.abort_id).await?
        && matches!(existing.status.as_str(), "finalized" | "resumed")
        && existing.finalization_reason.as_deref() == Some("acknowledged")
    {
        if existing.run_id != run_id || existing.resume_metadata.0 != command.resume_metadata {
            return Err(ExecutionError::AbortNotPending(command.abort_id));
        }
        let run = fetch_run(&mut transaction, run_id).await?;
        let value = RunAbortAcknowledged {
            abort: existing.try_into()?,
            run,
        };
        transaction.commit().await?;
        return Ok(AbortOutcome {
            value,
            created: false,
        });
    }

    let run = lock_run(&mut transaction, run_id).await?;
    if run_status(&run)? != RunStatus::Aborting {
        return Err(ExecutionError::AbortNotPending(command.abort_id));
    }
    let lease = lock_lease(&mut transaction, run_id).await?;
    verify_lease(&lease, command.lease_version, token)?;
    require_state_version(&run, command.expected_state_version)?;
    let abort = lock_abort(&mut transaction, command.abort_id).await?;
    if abort.run_id != run_id || abort.status != "pending" {
        return Err(ExecutionError::AbortNotPending(command.abort_id));
    }
    if abort.delivered_at.is_none() {
        return Err(ExecutionError::AbortNotDelivered(command.abort_id));
    }
    let query = format!(
        "update run_aborts set status = 'finalized', finalized_at = now(), \
         finalization_reason = 'acknowledged', resume_metadata = $2 \
         where abort_id = $1 returning {ABORT_COLUMNS}"
    );
    let abort = sqlx::query_as::<_, AbortRow>(&query)
        .bind(command.abort_id)
        .bind(Json(command.resume_metadata))
        .fetch_one(&mut *transaction)
        .await?;
    let run = finalize_run(&mut transaction, run_id).await?;
    delete_lease(&mut transaction, run_id).await?;
    let value = RunAbortAcknowledged {
        abort: abort.try_into()?,
        run: run.try_into()?,
    };
    transaction.commit().await?;
    Ok(AbortOutcome {
        value,
        created: true,
    })
}

pub(super) async fn resume(
    database: &Database,
    run_id: Uuid,
    command: ResumeRun,
) -> Result<AbortOutcome<RunResumed>, ExecutionError> {
    command.validate()?;
    let mut transaction = database.pool().begin().await?;
    let run = lock_run(&mut transaction, run_id).await?;
    let abort = latest_abort(&mut transaction, run_id, true)
        .await?
        .ok_or(ExecutionError::NoFinalizedAbort(run_id))?;
    if abort.status == "resumed" {
        if abort.resolution.as_ref().map(|value| &value.0) != Some(&command.resolution) {
            return Err(ExecutionError::AbortAlreadyResumed(run_id));
        }
        let value = RunResumed {
            abort: abort.try_into()?,
            run: run.try_into()?,
        };
        transaction.commit().await?;
        return Ok(AbortOutcome {
            value,
            created: false,
        });
    }
    if abort.status != "finalized" {
        return Err(ExecutionError::NoFinalizedAbort(run_id));
    }
    let status = run_status(&run)?;
    if status != RunStatus::Aborted {
        return Err(ExecutionError::RunNotResumable { run_id, status });
    }
    require_state_version(&run, command.expected_state_version)?;
    if run.current_turn >= run.max_turns {
        return Err(ExecutionError::RunTurnLimitReached {
            run_id,
            max_turns: u32::try_from(run.max_turns)
                .map_err(|error| invalid("runs.max_turns", error))?,
        });
    }
    let _: i64 = sqlx::query_scalar(
        "select current_revision from sessions where session_id = $1 for update",
    )
    .bind(run.session_id)
    .fetch_one(&mut *transaction)
    .await?;
    let has_later_run: bool = sqlx::query_scalar(
        "select exists(select 1 from runs where session_id = $1 \
         and (created_at, run_id) > ($2, $3))",
    )
    .bind(run.session_id)
    .bind(run.created_at)
    .bind(run.run_id)
    .fetch_one(&mut *transaction)
    .await?;
    if has_later_run {
        return Err(ExecutionError::RunNotLatest(run_id));
    }
    let has_active_run: bool = sqlx::query_scalar(
        "select exists(select 1 from runs where session_id = $1 and run_id <> $2 \
         and status in ('queued', 'running', 'waiting', 'aborting'))",
    )
    .bind(run.session_id)
    .bind(run_id)
    .fetch_one(&mut *transaction)
    .await?;
    if has_active_run {
        return Err(ExecutionError::SessionHasActiveRun(run.session_id));
    }
    let next_turn = run
        .current_turn
        .checked_add(1)
        .ok_or_else(|| invalid("runs.current_turn", "overflow"))?;
    let query = format!(
        "update run_aborts set status = 'resumed', resolution = $2, resumed_at = now(), \
         resumed_turn_number = $3 where abort_id = $1 returning {ABORT_COLUMNS}"
    );
    let abort = sqlx::query_as::<_, AbortRow>(&query)
        .bind(abort.abort_id)
        .bind(Json(command.resolution))
        .bind(next_turn)
        .fetch_one(&mut *transaction)
        .await?;
    let query = format!(
        "update runs set status = 'queued', current_turn = $2, failures_in_current_turn = 0, \
         state_version = state_version + 1, queued_at = now(), finished_at = null \
         where run_id = $1 returning {RUN_COLUMNS}"
    );
    let run = sqlx::query_as::<_, RunRow>(&query)
        .bind(run_id)
        .bind(next_turn)
        .fetch_one(&mut *transaction)
        .await?;
    let value = RunResumed {
        abort: abort.try_into()?,
        run: run.try_into()?,
    };
    transaction.commit().await?;
    Ok(AbortOutcome {
        value,
        created: true,
    })
}

pub(super) async fn deliver_pending(
    transaction: &mut Transaction<'_, Postgres>,
    run_id: Uuid,
) -> Result<AbortDirective, ExecutionError> {
    let query = format!(
        "update run_aborts set delivered_at = coalesce(delivered_at, now()) \
         where abort_id = (select abort_id from run_aborts \
             where run_id = $1 and status = 'pending' for update) \
         returning {ABORT_COLUMNS}"
    );
    let row = sqlx::query_as::<_, AbortRow>(&query)
        .bind(run_id)
        .fetch_optional(&mut **transaction)
        .await?
        .ok_or(ExecutionError::AbortInProgress(run_id))?;
    Ok(AbortDirective {
        abort_id: row.abort_id,
        reason: row.reason,
        payload: row.payload.0,
        requested_at: row.requested_at,
        deadline_at: row.deadline_at,
    })
}

pub(super) async fn finalize_expired(
    transaction: &mut Transaction<'_, Postgres>,
    run: RunRow,
    _lease: &LeaseRow,
) -> Result<(), ExecutionError> {
    let abort = latest_abort(transaction, run.run_id, true)
        .await?
        .ok_or(ExecutionError::AbortInProgress(run.run_id))?;
    if abort.status != "pending" {
        return Err(ExecutionError::AbortInProgress(run.run_id));
    }
    let reason = if abort.deadline_at <= Utc::now() {
        AbortFinalizationReason::DeadlineExpired
    } else {
        AbortFinalizationReason::LeaseExpired
    };
    sqlx::query(
        "update run_aborts set status = 'finalized', finalized_at = now(), \
         finalization_reason = $2 where abort_id = $1",
    )
    .bind(abort.abort_id)
    .bind(reason.as_db())
    .execute(&mut **transaction)
    .await?;
    finalize_run(transaction, run.run_id).await?;
    delete_lease(transaction, run.run_id).await?;
    Ok(())
}

pub(super) async fn resume_for_claim(
    transaction: &mut Transaction<'_, Postgres>,
    run: &RunRow,
) -> Result<Option<RunResume>, ExecutionError> {
    let row = sqlx::query_as::<_, AbortRow>(&format!(
        "select {ABORT_COLUMNS} from run_aborts \
         where run_id = $1 and status = 'resumed' and resumed_turn_number = $2 \
         order by sequence desc limit 1"
    ))
    .bind(run.run_id)
    .bind(run.current_turn)
    .fetch_optional(&mut **transaction)
    .await?;
    row.map(|row| row.resume().map(RunResume::Abort))
        .transpose()
}

pub(super) async fn get(database: &Database, abort_id: Uuid) -> Result<RunAbort, ExecutionError> {
    let mut transaction = database.pool().begin().await?;
    let row = find_abort(&mut transaction, abort_id)
        .await?
        .ok_or(ExecutionError::AbortNotFound(abort_id))?;
    let abort = row.try_into()?;
    transaction.commit().await?;
    Ok(abort)
}

pub(super) async fn list(
    database: &Database,
    run_id: Uuid,
    query: AbortListQuery,
) -> Result<RunAbortPage, ExecutionError> {
    let limit = query.limit.unwrap_or(DEFAULT_PAGE_SIZE);
    if !(1..=MAX_PAGE_SIZE).contains(&limit) {
        return Err(ExecutionError::InvalidAbortPageSize);
    }
    let cursor = query
        .cursor
        .as_deref()
        .map(|value| value.parse::<i64>())
        .transpose()
        .map_err(|_| ExecutionError::InvalidAbortCursor)?;
    let mut sql = QueryBuilder::<Postgres>::new(format!(
        "select {ABORT_COLUMNS} from run_aborts where run_id = "
    ));
    sql.push_bind(run_id);
    if let Some(status) = query.status {
        sql.push(" and status = ").push_bind(status.as_db());
    }
    if let Some(cursor) = cursor {
        sql.push(" and sequence > ").push_bind(cursor);
    }
    sql.push(" order by sequence limit ")
        .push_bind(i64::from(limit) + 1);
    let mut rows = sql
        .build_query_as::<AbortRow>()
        .fetch_all(database.pool())
        .await?;
    let has_more = rows.len() > limit as usize;
    rows.truncate(limit as usize);
    let next_cursor = has_more.then(|| {
        rows.last()
            .expect("page with more aborts is non-empty")
            .sequence
            .to_string()
    });
    let items = rows
        .into_iter()
        .map(RunAbort::try_from)
        .collect::<Result<Vec<_>, _>>()?;
    Ok(RunAbortPage { items, next_cursor })
}

async fn update_aborted_state(
    transaction: &mut Transaction<'_, Postgres>,
    run_id: Uuid,
    pending: bool,
) -> Result<RunRow, ExecutionError> {
    let query = if pending {
        format!(
            "update runs set status = 'aborting', state_version = state_version + 1 \
             where run_id = $1 returning {RUN_COLUMNS}"
        )
    } else {
        format!(
            "update runs set status = 'aborted', state_version = state_version + 1, \
             queued_at = null, finished_at = now() where run_id = $1 returning {RUN_COLUMNS}"
        )
    };
    sqlx::query_as::<_, RunRow>(&query)
        .bind(run_id)
        .fetch_one(&mut **transaction)
        .await
        .map_err(Into::into)
}

async fn finalize_run(
    transaction: &mut Transaction<'_, Postgres>,
    run_id: Uuid,
) -> Result<RunRow, ExecutionError> {
    let query = format!(
        "update runs set status = 'aborted', state_version = state_version + 1, \
         finished_at = now() where run_id = $1 returning {RUN_COLUMNS}"
    );
    sqlx::query_as::<_, RunRow>(&query)
        .bind(run_id)
        .fetch_one(&mut **transaction)
        .await
        .map_err(Into::into)
}

async fn next_sequence(
    transaction: &mut Transaction<'_, Postgres>,
    run_id: Uuid,
) -> Result<i64, ExecutionError> {
    let current = sqlx::query_scalar::<_, Option<i64>>(
        "select max(sequence) from run_aborts where run_id = $1",
    )
    .bind(run_id)
    .fetch_one(&mut **transaction)
    .await?
    .unwrap_or(0);
    current
        .checked_add(1)
        .ok_or_else(|| invalid("run_aborts.sequence", "overflow"))
}

async fn latest_abort(
    transaction: &mut Transaction<'_, Postgres>,
    run_id: Uuid,
    lock: bool,
) -> Result<Option<AbortRow>, ExecutionError> {
    let suffix = if lock { " for update" } else { "" };
    sqlx::query_as::<_, AbortRow>(&format!(
        "select {ABORT_COLUMNS} from run_aborts where run_id = $1 \
         order by sequence desc limit 1{suffix}"
    ))
    .bind(run_id)
    .fetch_optional(&mut **transaction)
    .await
    .map_err(Into::into)
}

async fn find_abort(
    transaction: &mut Transaction<'_, Postgres>,
    abort_id: Uuid,
) -> Result<Option<AbortRow>, ExecutionError> {
    sqlx::query_as::<_, AbortRow>(&format!(
        "select {ABORT_COLUMNS} from run_aborts where abort_id = $1"
    ))
    .bind(abort_id)
    .fetch_optional(&mut **transaction)
    .await
    .map_err(Into::into)
}

async fn lock_abort(
    transaction: &mut Transaction<'_, Postgres>,
    abort_id: Uuid,
) -> Result<AbortRow, ExecutionError> {
    sqlx::query_as::<_, AbortRow>(&format!(
        "select {ABORT_COLUMNS} from run_aborts where abort_id = $1 for update"
    ))
    .bind(abort_id)
    .fetch_optional(&mut **transaction)
    .await?
    .ok_or(ExecutionError::AbortNotFound(abort_id))
}

async fn fetch_run(
    transaction: &mut Transaction<'_, Postgres>,
    run_id: Uuid,
) -> Result<Run, ExecutionError> {
    let row =
        sqlx::query_as::<_, RunRow>(&format!("select {RUN_COLUMNS} from runs where run_id = $1"))
            .bind(run_id)
            .fetch_optional(&mut **transaction)
            .await?
            .ok_or(ExecutionError::RunNotFound(run_id))?;
    row.try_into()
}

fn run_status(run: &RunRow) -> Result<RunStatus, ExecutionError> {
    RunStatus::from_db(&run.status).ok_or_else(|| invalid("runs.status", &run.status))
}

fn invalid(field: &str, error: impl std::fmt::Display) -> ExecutionError {
    ExecutionError::InvalidStoredData(format!("{field}: {error}"))
}
