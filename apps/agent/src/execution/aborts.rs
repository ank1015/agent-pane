use agent_contracts::{
    HARNESS_PROTOCOL_VERSION, RunCancelled, RunEventData, TurnEndReason, cancelled_subject,
};
use chrono::Utc;
use llm_contracts::Validate as _;
use sqlx::types::Json;
use uuid::Uuid;

use super::{
    AbortListQuery, AbortRow, ExecutionError, RequestRunAbort, RunAbort, RunAbortPage,
    RunAbortResult, RunRow, RunStatus, constraint, events, outbox,
    records::{RUN_COLUMNS, positive_u32, positive_u64},
    runs,
};
use crate::db::Database;

const ABORT_COLUMNS: &str = "abort_id, run_id, turn_number, reason, payload, requested_at";

pub(super) async fn request(
    database: &Database,
    run_id: Uuid,
    request: RequestRunAbort,
) -> Result<super::CreateOutcome<RunAbortResult>, ExecutionError> {
    request.validate()?;
    let mut tx = database.pool().begin().await?;
    if let Some(row) = sqlx::query_as::<_, AbortRow>(&format!(
        "select {ABORT_COLUMNS} from run_aborts where run_id = $1"
    ))
    .bind(run_id)
    .fetch_optional(&mut *tx)
    .await?
    {
        let abort = RunAbort::try_from(row)?;
        if abort.abort_id != request.abort_id {
            return Err(ExecutionError::AbortIdConflict(request.abort_id));
        }
        let run_row = sqlx::query_as::<_, RunRow>(&format!(
            "select {RUN_COLUMNS} from runs where run_id = $1"
        ))
        .bind(run_id)
        .fetch_one(&mut *tx)
        .await?;
        tx.commit().await?;
        return Ok(super::CreateOutcome {
            value: RunAbortResult {
                abort,
                run: run_row.try_into()?,
            },
            created: false,
        });
    }
    let context = runs::load_context(&mut tx, run_id, true).await?;
    let status = RunStatus::from_db(&context.status).ok_or_else(|| {
        ExecutionError::InvalidStoredData(format!("runs.status: {}", context.status))
    })?;
    if !matches!(status, RunStatus::Active | RunStatus::Waiting) {
        return Err(ExecutionError::RunNotAbortable { run_id, status });
    }
    let actual = u64::try_from(context.state_version)
        .map_err(|e| ExecutionError::InvalidStoredData(format!("runs.state_version: {e}")))?;
    if request.expected_state_version != actual {
        return Err(ExecutionError::RunStateConflict {
            expected: request.expected_state_version,
            actual,
        });
    }
    let row = sqlx::query_as::<_, AbortRow>(&format!("insert into run_aborts (abort_id, run_id, turn_number, reason, payload) values ($1,$2,$3,$4,$5) returning {ABORT_COLUMNS}"))
        .bind(request.abort_id).bind(run_id).bind(context.current_turn).bind(&request.reason).bind(Json(&request.payload)).fetch_one(&mut *tx).await
        .map_err(|error| if matches!(constraint(&error), Some("run_aborts_pkey" | "run_aborts_one_per_run")) { ExecutionError::AbortIdConflict(request.abort_id) } else { error.into() })?;
    sqlx::query("update run_waits set status = 'cancelled', cancelled_at = now() where run_id = $1 and status = 'pending'").bind(run_id).execute(&mut *tx).await?;
    sqlx::query("update session_messages set state = 'discarded', discard_reason = 'run_aborted', discarded_at = now() where run_id = $1 and delivery = 'next_turn' and state = 'pending'").bind(run_id).execute(&mut *tx).await?;
    sqlx::query("update runs set status = 'aborted', state_version = state_version + 1, finished_at = now() where run_id = $1").bind(run_id).execute(&mut *tx).await?;
    let updated = runs::load_context(&mut tx, run_id, false).await?;
    let emitted_at = Utc::now();
    let updated_turn = positive_u32("runs.current_turn", updated.current_turn)?;
    let updated_state = positive_u64("runs.state_version", updated.state_version)?;
    events::append_agent(
        &mut tx,
        run_id,
        Some(updated_turn),
        updated_state,
        RunStatus::Aborted,
        Uuid::now_v7(),
        emitted_at,
        RunEventData::TurnEnded {
            reason: TurnEndReason::Aborted,
        },
    )
    .await?;
    events::append_agent(
        &mut tx,
        run_id,
        Some(updated_turn),
        updated_state,
        RunStatus::Aborted,
        Uuid::now_v7(),
        emitted_at,
        RunEventData::RunAborted {
            abort_id: request.abort_id,
            reason: request.reason.clone(),
        },
    )
    .await?;
    let event_id = Uuid::now_v7();
    let event = RunCancelled {
        protocol_version: HARNESS_PROTOCOL_VERSION,
        event_id,
        emitted_at,
        run_id,
        turn_number: u32::try_from(updated.current_turn)
            .map_err(|e| ExecutionError::InvalidStoredData(format!("runs.current_turn: {e}")))?,
        state_version: u64::try_from(updated.state_version)
            .map_err(|e| ExecutionError::InvalidStoredData(format!("runs.state_version: {e}")))?,
        reason: request.reason,
        payload: request.payload,
    };
    outbox::enqueue(
        &mut tx,
        event_id,
        &cancelled_subject(&context.harness_slug),
        &event,
    )
    .await?;
    let run_row =
        sqlx::query_as::<_, RunRow>(&format!("select {RUN_COLUMNS} from runs where run_id = $1"))
            .bind(run_id)
            .fetch_one(&mut *tx)
            .await?;
    let value = RunAbortResult {
        abort: row.try_into()?,
        run: run_row.try_into()?,
    };
    tx.commit().await?;
    Ok(super::CreateOutcome {
        value,
        created: true,
    })
}

pub(super) async fn get(
    database: &Database,
    run_id: Uuid,
    abort_id: Uuid,
) -> Result<RunAbort, ExecutionError> {
    let row = sqlx::query_as::<_, AbortRow>(&format!(
        "select {ABORT_COLUMNS} from run_aborts where run_id = $1 and abort_id = $2"
    ))
    .bind(run_id)
    .bind(abort_id)
    .fetch_optional(database.pool())
    .await?
    .ok_or(ExecutionError::AbortNotFound(abort_id))?;
    row.try_into()
}

pub(super) async fn list(
    database: &Database,
    run_id: Uuid,
    query: AbortListQuery,
) -> Result<RunAbortPage, ExecutionError> {
    runs::get(database, run_id).await?;
    if query.cursor.is_some() {
        return Err(ExecutionError::InvalidAbortCursor);
    }
    let limit = query.limit.unwrap_or(50);
    if !(1..=100).contains(&limit) {
        return Err(ExecutionError::InvalidAbortPageSize);
    }
    let items = sqlx::query_as::<_, AbortRow>(&format!("select {ABORT_COLUMNS} from run_aborts where run_id = $1 order by requested_at, abort_id limit $2"))
        .bind(run_id).bind(i64::from(limit)).fetch_all(database.pool()).await?.into_iter().map(RunAbort::try_from).collect::<Result<Vec<_>, _>>()?;
    Ok(RunAbortPage {
        items,
        next_cursor: None,
    })
}
