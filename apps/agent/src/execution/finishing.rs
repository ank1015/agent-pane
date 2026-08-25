use llm_contracts::{JsonObject, Validate as _};
use serde_json::Value;
use sqlx::{Postgres, Transaction, types::Json};
use uuid::Uuid;

use super::{
    CompleteRunTurn, ExecutionError, FailRunTurn, Run, RunTurnCompleted, RunTurnFailed,
    TurnCompletionDisposition,
    leasing::{LeaseToken, lock_lease, lock_run, require_running, verify_lease},
    records::{COMMITTED_MESSAGE_COLUMNS, CommittedMessageRow, RUN_COLUMNS, RunRow},
};
use crate::{db::Database, sessions::SessionMessage};

pub(super) async fn complete(
    database: &Database,
    run_id: Uuid,
    command: CompleteRunTurn,
    token: &LeaseToken,
) -> Result<RunTurnCompleted, ExecutionError> {
    command.validate()?;
    let mut transaction = database.pool().begin().await?;
    let run = lock_run(&mut transaction, run_id).await?;
    require_running(&run)?;
    let lease = lock_lease(&mut transaction, run_id).await?;
    verify_lease(&lease, command.lease_version, token)?;
    require_state_version(&run, command.expected_state_version)?;
    let current_revision = lock_session(&mut transaction, run.session_id).await?;

    let (run, committed_messages) = match command.disposition {
        TurnCompletionDisposition::Continue => {
            if run.current_turn >= run.max_turns {
                return Err(ExecutionError::RunTurnLimitReached {
                    run_id,
                    max_turns: u32::try_from(run.max_turns).map_err(|error| {
                        ExecutionError::InvalidStoredData(format!("runs.max_turns: {error}"))
                    })?,
                });
            }
            let next_turn = run
                .current_turn
                .checked_add(1)
                .ok_or_else(|| ExecutionError::InvalidStoredData("run turn overflow".to_owned()))?;
            let (revision, messages) =
                commit_pending_messages(&mut transaction, &run, next_turn, current_revision)
                    .await?;
            if revision != current_revision {
                sqlx::query("update sessions set current_revision = $2 where session_id = $1")
                    .bind(run.session_id)
                    .bind(revision)
                    .execute(&mut *transaction)
                    .await?;
            }
            let query = format!(
                "update runs set status = 'queued', current_turn = $2, \
                 failures_in_current_turn = 0, state_version = state_version + 1, queued_at = now() \
                 where run_id = $1 returning {RUN_COLUMNS}"
            );
            let updated = sqlx::query_as::<_, RunRow>(&query)
                .bind(run_id)
                .bind(next_turn)
                .fetch_one(&mut *transaction)
                .await?;
            (updated, messages)
        }
        TurnCompletionDisposition::Complete => {
            let final_message_id = command
                .final_message_id
                .expect("validated complete disposition has a final message");
            let valid: bool = sqlx::query_scalar(
                "select exists( \
                   select 1 from session_messages \
                   where session_message_id = $1 and session_id = $2 and state = 'committed' \
                     and origin = 'harness' and run_id = $3 and turn_number = $4 \
                 )",
            )
            .bind(final_message_id)
            .bind(run.session_id)
            .bind(run_id)
            .bind(run.current_turn)
            .fetch_one(&mut *transaction)
            .await?;
            if !valid {
                return Err(ExecutionError::InvalidFinalMessage);
            }
            discard_pending_messages(&mut transaction, run_id, "run_completed").await?;
            let query = format!(
                "update runs set status = 'completed', state_version = state_version + 1, \
                 final_message_id = $2, finished_at = now() \
                 where run_id = $1 returning {RUN_COLUMNS}"
            );
            let updated = sqlx::query_as::<_, RunRow>(&query)
                .bind(run_id)
                .bind(final_message_id)
                .fetch_one(&mut *transaction)
                .await?;
            (updated, Vec::new())
        }
    };
    delete_lease(&mut transaction, run_id).await?;
    let outcome = RunTurnCompleted {
        run: Run::try_from(run)?,
        committed_messages,
    };
    transaction.commit().await?;
    Ok(outcome)
}

pub(super) async fn fail(
    database: &Database,
    run_id: Uuid,
    command: FailRunTurn,
    token: &LeaseToken,
) -> Result<RunTurnFailed, ExecutionError> {
    command.validate()?;
    let mut transaction = database.pool().begin().await?;
    let run = lock_run(&mut transaction, run_id).await?;
    require_running(&run)?;
    let lease = lock_lease(&mut transaction, run_id).await?;
    verify_lease(&lease, command.lease_version, token)?;
    require_state_version(&run, command.expected_state_version)?;
    let (run, will_retry) = fail_locked(&mut transaction, run, command.failure).await?;
    let outcome = RunTurnFailed {
        run: run.try_into()?,
        will_retry,
    };
    transaction.commit().await?;
    Ok(outcome)
}

pub(super) async fn fail_locked(
    transaction: &mut Transaction<'_, Postgres>,
    run: RunRow,
    failure: JsonObject,
) -> Result<(RunRow, bool), ExecutionError> {
    let failures = run.failures_in_current_turn.checked_add(1).ok_or_else(|| {
        ExecutionError::InvalidStoredData("run failure counter overflow".to_owned())
    })?;
    let will_retry = failures < run.max_failures_per_turn;
    let updated = if will_retry {
        let query = format!(
            "update runs set status = 'queued', failures_in_current_turn = $2, \
             state_version = state_version + 1, queued_at = now() \
             where run_id = $1 returning {RUN_COLUMNS}"
        );
        sqlx::query_as::<_, RunRow>(&query)
            .bind(run.run_id)
            .bind(failures)
            .fetch_one(&mut **transaction)
            .await?
    } else {
        discard_pending_messages(transaction, run.run_id, "run_failed").await?;
        let query = format!(
            "update runs set status = 'failed', failures_in_current_turn = $2, \
             state_version = state_version + 1, failure = $3, finished_at = now() \
             where run_id = $1 returning {RUN_COLUMNS}"
        );
        sqlx::query_as::<_, RunRow>(&query)
            .bind(run.run_id)
            .bind(failures)
            .bind(Json(Value::Object(failure)))
            .fetch_one(&mut **transaction)
            .await?
    };
    delete_lease(transaction, run.run_id).await?;
    Ok((updated, will_retry))
}

pub(super) fn require_state_version(run: &RunRow, expected: u64) -> Result<(), ExecutionError> {
    let actual = super::records::positive_u64("runs.state_version", run.state_version)?;
    if actual != expected {
        return Err(ExecutionError::RunStateConflict { expected, actual });
    }
    Ok(())
}

async fn lock_session(
    transaction: &mut Transaction<'_, Postgres>,
    session_id: Uuid,
) -> Result<i64, ExecutionError> {
    sqlx::query_scalar("select current_revision from sessions where session_id = $1 for update")
        .bind(session_id)
        .fetch_one(&mut **transaction)
        .await
        .map_err(Into::into)
}

async fn commit_pending_messages(
    transaction: &mut Transaction<'_, Postgres>,
    run: &RunRow,
    next_turn: i32,
    mut revision: i64,
) -> Result<(i64, Vec<SessionMessage>), ExecutionError> {
    let ids = sqlx::query_scalar::<_, Uuid>(
        "select session_message_id from session_messages \
         where run_id = $1 and state = 'pending' order by queue_sequence for update",
    )
    .bind(run.run_id)
    .fetch_all(&mut **transaction)
    .await?;
    let mut messages = Vec::with_capacity(ids.len());
    for id in ids {
        revision = revision.checked_add(1).ok_or_else(|| {
            ExecutionError::InvalidStoredData("session revision overflow".to_owned())
        })?;
        let query = format!(
            "update session_messages set state = 'committed', revision = $2, \
             turn_number = $3, committed_at = now() \
             where session_message_id = $1 returning {COMMITTED_MESSAGE_COLUMNS}"
        );
        let row = sqlx::query_as::<_, CommittedMessageRow>(&query)
            .bind(id)
            .bind(revision)
            .bind(next_turn)
            .fetch_one(&mut **transaction)
            .await?;
        messages.push(row.try_into()?);
    }
    Ok((revision, messages))
}

pub(super) async fn discard_pending_messages(
    transaction: &mut Transaction<'_, Postgres>,
    run_id: Uuid,
    reason: &str,
) -> Result<(), ExecutionError> {
    sqlx::query(
        "update session_messages set state = 'discarded', discard_reason = $2, discarded_at = now() \
         where run_id = $1 and state = 'pending'",
    )
    .bind(run_id)
    .bind(reason)
    .execute(&mut **transaction)
    .await?;
    Ok(())
}

pub(super) async fn delete_lease(
    transaction: &mut Transaction<'_, Postgres>,
    run_id: Uuid,
) -> Result<(), ExecutionError> {
    sqlx::query("delete from run_leases where run_id = $1")
        .bind(run_id)
        .execute(&mut **transaction)
        .await?;
    Ok(())
}
