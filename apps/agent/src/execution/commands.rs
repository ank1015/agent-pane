use agent_contracts::{
    AppliedHarnessCommand, HARNESS_PROTOCOL_VERSION, HarnessCommand, HarnessCommandOutcome,
    HarnessCommandResult, HarnessOperation, RejectedHarnessCommand, RunStatus, result_subject,
};
use chrono::Utc;
use llm_contracts::Validate as _;
use serde_json::Value;
use sha2::{Digest, Sha256};
use sqlx::{FromRow, Postgres, Transaction, types::Json};
use uuid::Uuid;

use super::{ExecutionError, constraint, outbox, records::positive_u64, runs};
use crate::db::Database;

#[derive(FromRow)]
struct InboxRow {
    payload_hash: Vec<u8>,
    result: Json<Value>,
}

pub async fn apply_harness_command(
    database: &Database,
    command: &HarnessCommand,
    raw_payload: &[u8],
) -> Result<HarnessCommandResult, ExecutionError> {
    command.validate()?;
    let payload_hash = Sha256::digest(raw_payload).to_vec();
    let mut tx = database.pool().begin().await?;
    if let Some(existing) = sqlx::query_as::<_, InboxRow>(
        "select payload_hash, result from broker_inbox where command_id = $1",
    )
    .bind(command.command_id)
    .fetch_optional(&mut *tx)
    .await?
    {
        let mut result: HarnessCommandResult =
            serde_json::from_value(existing.result.0).map_err(|error| {
                ExecutionError::InvalidStoredData(format!("broker_inbox.result: {error}"))
            })?;
        if existing.payload_hash != payload_hash {
            result = rejected(
                command,
                "command_id_conflict",
                "command_id was already used with a different payload",
                None,
            );
        } else if let HarnessCommandOutcome::Applied(applied) = result.outcome {
            result.result_id = Uuid::now_v7();
            result.emitted_at = Utc::now();
            result.outcome = HarnessCommandOutcome::Duplicate(applied);
        }
        outbox::enqueue(
            &mut tx,
            result.result_id,
            &result_subject(&command.harness_slug),
            &result,
        )
        .await?;
        tx.commit().await?;
        return Ok(result);
    }

    let outcome = match apply_operation(&mut tx, command).await {
        Ok(applied) => HarnessCommandOutcome::Applied(applied),
        Err(error) if is_rejection(&error) => rejection_from_error(error),
        Err(error) => return Err(error),
    };
    let result = HarnessCommandResult {
        protocol_version: HARNESS_PROTOCOL_VERSION,
        result_id: Uuid::now_v7(),
        command_id: command.command_id,
        emitted_at: Utc::now(),
        run_id: command.run_id,
        outcome,
    };
    sqlx::query("insert into broker_inbox (command_id, payload_hash, result) values ($1, $2, $3)")
        .bind(command.command_id)
        .bind(&payload_hash)
        .bind(Json(serde_json::to_value(&result).map_err(|error| {
            ExecutionError::InvalidStoredData(format!("command result serialization: {error}"))
        })?))
        .execute(&mut *tx)
        .await
        .map_err(|error| {
            if constraint(&error) == Some("broker_inbox_pkey") {
                ExecutionError::CommandIdConflict(command.command_id)
            } else {
                error.into()
            }
        })?;
    outbox::enqueue(
        &mut tx,
        result.result_id,
        &result_subject(&command.harness_slug),
        &result,
    )
    .await?;
    if matches!(&command.operation, HarnessOperation::Continue)
        && matches!(&result.outcome, HarnessCommandOutcome::Applied(_))
    {
        let next = runs::load_context(&mut tx, command.run_id, false).await?;
        runs::enqueue_turn(&mut tx, &next, None).await?;
    }
    tx.commit().await?;
    Ok(result)
}

async fn apply_operation(
    tx: &mut Transaction<'_, Postgres>,
    command: &HarnessCommand,
) -> Result<AppliedHarnessCommand, ExecutionError> {
    let context = runs::load_context(tx, command.run_id, true).await?;
    runs::require_active(
        &context,
        command.expected_state_version,
        command.turn_number,
    )?;
    if context.harness_slug != command.harness_slug {
        return Err(ExecutionError::HarnessNotAvailable(
            command.harness_slug.clone(),
        ));
    }
    match &command.operation {
        HarnessOperation::Complete { final_message_id } => {
            complete(tx, &context, *final_message_id).await?
        }
        HarnessOperation::Continue => continue_run(tx, &context).await?,
        HarnessOperation::Fail { failure } => {
            sqlx::query("update runs set status = 'failed', failure = $2, state_version = state_version + 1, finished_at = now() where run_id = $1")
                .bind(command.run_id).bind(Json(failure)).execute(&mut **tx).await?;
            discard_pending(tx, command.run_id, "run_failed").await?;
        }
        HarnessOperation::Wait(wait) => {
            let inserted = sqlx::query("insert into run_waits (wait_id, run_id, turn_number, harness_wait_id, kind, public_request, resume_metadata, expires_at) values ($1,$2,$3,$4,$5,$6,$7,$8)")
                .bind(wait.wait_id).bind(command.run_id).bind(context.current_turn).bind(&wait.harness_wait_id).bind(&wait.kind)
                .bind(Json(&wait.public_request)).bind(Json(&wait.resume_metadata)).bind(wait.expires_at).execute(&mut **tx).await;
            if let Err(error) = inserted {
                return Err(match constraint(&error) {
                    Some("run_waits_pkey") => ExecutionError::WaitIdConflict(wait.wait_id),
                    Some("run_waits_harness_id_unique") => {
                        ExecutionError::HarnessWaitIdConflict(wait.harness_wait_id.clone())
                    }
                    Some("run_waits_one_pending_per_run_idx") => {
                        ExecutionError::RunAlreadyWaiting(command.run_id)
                    }
                    _ => error.into(),
                });
            }
            sqlx::query("update runs set status = 'waiting', state_version = state_version + 1 where run_id = $1")
                .bind(command.run_id).execute(&mut **tx).await?;
        }
    }
    applied_state(tx, command.run_id).await
}

async fn complete(
    tx: &mut Transaction<'_, Postgres>,
    context: &super::records::RunContextRow,
    final_message_id: Uuid,
) -> Result<(), ExecutionError> {
    let valid = sqlx::query_scalar::<_, bool>("select exists(select 1 from session_messages where session_message_id = $1 and session_id = $2 and run_id = $3 and turn_number = $4 and origin = 'harness' and state = 'committed')")
        .bind(final_message_id).bind(context.session_id).bind(context.run_id).bind(context.current_turn).fetch_one(&mut **tx).await?;
    if !valid {
        return Err(ExecutionError::InvalidFinalMessage);
    }
    sqlx::query("update runs set status = 'completed', final_message_id = $2, state_version = state_version + 1, finished_at = now() where run_id = $1")
        .bind(context.run_id).bind(final_message_id).execute(&mut **tx).await?;
    discard_pending(tx, context.run_id, "run_completed").await
}

async fn continue_run(
    tx: &mut Transaction<'_, Postgres>,
    context: &super::records::RunContextRow,
) -> Result<(), ExecutionError> {
    if context.current_turn >= context.max_turns {
        return Err(ExecutionError::RunTurnLimitReached {
            run_id: context.run_id,
            max_turns: u32::try_from(context.max_turns).unwrap_or_default(),
        });
    }
    let ids = sqlx::query_scalar::<_, Uuid>("select session_message_id from session_messages where run_id = $1 and delivery = 'next_turn' and state = 'pending' order by queue_sequence for update")
        .bind(context.run_id).fetch_all(&mut **tx).await?;
    let mut revision = context.current_session_revision;
    let next_turn = context.current_turn + 1;
    for id in ids {
        revision += 1;
        sqlx::query("update session_messages set state = 'committed', revision = $2, turn_number = $3, committed_at = now() where session_message_id = $1")
            .bind(id).bind(revision).bind(next_turn).execute(&mut **tx).await?;
    }
    if revision != context.current_session_revision {
        sqlx::query("update sessions set current_revision = $2 where session_id = $1")
            .bind(context.session_id)
            .bind(revision)
            .execute(&mut **tx)
            .await?;
    }
    sqlx::query("update runs set current_turn = current_turn + 1, state_version = state_version + 1 where run_id = $1")
        .bind(context.run_id).execute(&mut **tx).await?;
    Ok(())
}

async fn discard_pending(
    tx: &mut Transaction<'_, Postgres>,
    run_id: Uuid,
    reason: &str,
) -> Result<(), ExecutionError> {
    sqlx::query("update session_messages set state = 'discarded', discard_reason = $2, discarded_at = now() where run_id = $1 and delivery = 'next_turn' and state = 'pending'")
        .bind(run_id).bind(reason).execute(&mut **tx).await?;
    Ok(())
}

async fn applied_state(
    tx: &mut Transaction<'_, Postgres>,
    run_id: Uuid,
) -> Result<AppliedHarnessCommand, ExecutionError> {
    let context = runs::load_context(tx, run_id, false).await?;
    Ok(AppliedHarnessCommand {
        run_state_version: positive_u64("runs.state_version", context.state_version)?,
        current_session_revision: u64::try_from(context.current_session_revision).map_err(|e| {
            ExecutionError::InvalidStoredData(format!("sessions.current_revision: {e}"))
        })?,
        run_status: RunStatus::from_db(&context.status).ok_or_else(|| {
            ExecutionError::InvalidStoredData(format!("runs.status: {}", context.status))
        })?,
    })
}

fn rejected(
    command: &HarnessCommand,
    code: &str,
    message: &str,
    actual: Option<u64>,
) -> HarnessCommandResult {
    HarnessCommandResult {
        protocol_version: HARNESS_PROTOCOL_VERSION,
        result_id: Uuid::now_v7(),
        command_id: command.command_id,
        emitted_at: Utc::now(),
        run_id: command.run_id,
        outcome: HarnessCommandOutcome::Rejected(RejectedHarnessCommand {
            code: code.to_owned(),
            message: message.to_owned(),
            actual_state_version: actual,
        }),
    }
}

fn is_rejection(error: &ExecutionError) -> bool {
    !matches!(
        error,
        ExecutionError::Database(_) | ExecutionError::InvalidStoredData(_)
    )
}

fn rejection_from_error(error: ExecutionError) -> HarnessCommandOutcome {
    let actual = match &error {
        ExecutionError::RunStateConflict { actual, .. } => Some(*actual),
        _ => None,
    };
    let code = match &error {
        ExecutionError::RunNotFound(_) => "run_not_found",
        ExecutionError::RunStateConflict { .. } => "run_state_conflict",
        ExecutionError::RunTurnConflict { .. } => "run_turn_conflict",
        ExecutionError::RunNotActive { .. } => "run_not_active",
        ExecutionError::RunTurnLimitReached { .. } => "run_turn_limit_reached",
        ExecutionError::InvalidFinalMessage => "invalid_final_message",
        ExecutionError::WaitIdConflict(_) => "wait_id_conflict",
        ExecutionError::HarnessWaitIdConflict(_) => "harness_wait_id_conflict",
        ExecutionError::RunAlreadyWaiting(_) => "run_already_waiting",
        ExecutionError::HarnessNotAvailable(_) => "harness_mismatch",
        _ => "command_rejected",
    };
    HarnessCommandOutcome::Rejected(RejectedHarnessCommand {
        code: code.to_owned(),
        message: error.to_string(),
        actual_state_version: actual,
    })
}
