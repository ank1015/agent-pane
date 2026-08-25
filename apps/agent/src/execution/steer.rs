use llm_contracts::Validate as _;
use sqlx::{Postgres, QueryBuilder, types::Json};
use uuid::Uuid;

use super::{
    ExecutionError, QueueRunMessage, QueuedRunMessage, QueuedRunMessageListQuery,
    QueuedRunMessagePage, RunStatus,
    leasing::lock_run,
    records::{QUEUED_MESSAGE_COLUMNS, QueuedMessageRow, positive_u64},
};
use crate::db::Database;

const DEFAULT_PAGE_SIZE: u32 = 50;
const MAX_PAGE_SIZE: u32 = 100;

pub(super) struct QueueOutcome {
    pub message: QueuedRunMessage,
    pub created: bool,
}

pub(super) async fn queue(
    database: &Database,
    run_id: Uuid,
    command: QueueRunMessage,
) -> Result<QueueOutcome, ExecutionError> {
    command.validate()?;
    let mut transaction = database.pool().begin().await?;
    let run = lock_run(&mut transaction, run_id).await?;

    let query = format!(
        "select {QUEUED_MESSAGE_COLUMNS} from session_messages \
         where session_message_id = $1 and origin = 'external' and delivery = 'next_turn'"
    );
    if let Some(row) = sqlx::query_as::<_, QueuedMessageRow>(&query)
        .bind(command.input.session_message_id)
        .fetch_optional(&mut *transaction)
        .await?
    {
        let message = QueuedRunMessage::try_from(row)?;
        if message.run_id != run_id
            || message.session_id != run.session_id
            || message.message != command.input.message
        {
            return Err(ExecutionError::MessageIdConflict(
                command.input.session_message_id,
            ));
        }
        transaction.commit().await?;
        return Ok(QueueOutcome {
            message,
            created: false,
        });
    }

    let status = RunStatus::from_db(&run.status).ok_or_else(|| {
        ExecutionError::InvalidStoredData(format!("unknown run status {:?}", run.status))
    })?;
    if !matches!(
        status,
        RunStatus::Queued | RunStatus::Running | RunStatus::Waiting
    ) {
        return Err(ExecutionError::RunNotRunnable { run_id, status });
    }
    if run.current_turn >= run.max_turns {
        return Err(ExecutionError::RunTurnLimitReached {
            run_id,
            max_turns: u32::try_from(run.max_turns).map_err(|error| {
                ExecutionError::InvalidStoredData(format!("runs.max_turns: {error}"))
            })?,
        });
    }
    let actual = positive_u64("runs.state_version", run.state_version)?;
    if actual != command.expected_state_version {
        return Err(ExecutionError::RunStateConflict {
            expected: command.expected_state_version,
            actual,
        });
    }
    let maximum = sqlx::query_scalar::<_, Option<i64>>(
        "select max(queue_sequence) from session_messages \
         where run_id = $1 and delivery = 'next_turn'",
    )
    .bind(run_id)
    .fetch_one(&mut *transaction)
    .await?
    .unwrap_or(0);
    let sequence = maximum.checked_add(1).ok_or_else(|| {
        ExecutionError::InvalidStoredData("run message queue sequence overflow".to_owned())
    })?;
    let query = format!(
        "insert into session_messages \
         (session_message_id, session_id, message, origin, delivery, state, run_id, \
          queued_during_turn, queue_sequence) \
         values ($1, $2, $3, 'external', 'next_turn', 'pending', $4, $5, $6) \
         returning {QUEUED_MESSAGE_COLUMNS}"
    );
    let row = sqlx::query_as::<_, QueuedMessageRow>(&query)
        .bind(command.input.session_message_id)
        .bind(run.session_id)
        .bind(Json(command.input.message))
        .bind(run_id)
        .bind(run.current_turn)
        .bind(sequence)
        .fetch_one(&mut *transaction)
        .await
        .map_err(|error| match super::constraint(&error) {
            Some("session_messages_pkey") => {
                ExecutionError::MessageIdConflict(command.input.session_message_id)
            }
            _ => ExecutionError::Database(error),
        })?;
    let message = row.try_into()?;
    transaction.commit().await?;
    Ok(QueueOutcome {
        message,
        created: true,
    })
}

pub(super) async fn get(
    database: &Database,
    run_id: Uuid,
    session_message_id: Uuid,
) -> Result<QueuedRunMessage, ExecutionError> {
    ensure_run_exists(database, run_id).await?;
    let query = format!(
        "select {QUEUED_MESSAGE_COLUMNS} from session_messages \
         where run_id = $1 and session_message_id = $2 and delivery = 'next_turn'"
    );
    let row = sqlx::query_as::<_, QueuedMessageRow>(&query)
        .bind(run_id)
        .bind(session_message_id)
        .fetch_optional(database.pool())
        .await?
        .ok_or(ExecutionError::QueuedMessageNotFound(session_message_id))?;
    row.try_into()
}

pub(super) async fn list(
    database: &Database,
    run_id: Uuid,
    query: QueuedRunMessageListQuery,
) -> Result<QueuedRunMessagePage, ExecutionError> {
    let limit = query.limit.unwrap_or(DEFAULT_PAGE_SIZE);
    if !(1..=MAX_PAGE_SIZE).contains(&limit) {
        return Err(ExecutionError::InvalidQueuedMessagePageSize);
    }
    let after_sequence = query
        .after_sequence
        .map(i64::try_from)
        .transpose()
        .map_err(|_| ExecutionError::InvalidQueuedMessageAfterSequence)?;
    ensure_run_exists(database, run_id).await?;

    let mut sql = QueryBuilder::<Postgres>::new(format!(
        "select {QUEUED_MESSAGE_COLUMNS} from session_messages \
         where run_id = "
    ));
    sql.push_bind(run_id).push(" and delivery = 'next_turn'");
    if let Some(state) = query.state {
        sql.push(" and state = ").push_bind(state.as_db());
    }
    if let Some(sequence) = after_sequence {
        sql.push(" and queue_sequence > ").push_bind(sequence);
    }
    sql.push(" order by queue_sequence limit ")
        .push_bind(i64::from(limit) + 1);

    let mut rows = sql
        .build_query_as::<QueuedMessageRow>()
        .fetch_all(database.pool())
        .await?;
    let has_more = rows.len() > limit as usize;
    rows.truncate(limit as usize);
    let items = rows
        .into_iter()
        .map(QueuedRunMessage::try_from)
        .collect::<Result<Vec<_>, _>>()?;
    let next_after_sequence = has_more.then(|| {
        items
            .last()
            .expect("a page with more queued messages is not empty")
            .queue_sequence
    });
    Ok(QueuedRunMessagePage {
        items,
        next_after_sequence,
    })
}

async fn ensure_run_exists(database: &Database, run_id: Uuid) -> Result<(), ExecutionError> {
    let exists: bool = sqlx::query_scalar("select exists(select 1 from runs where run_id = $1)")
        .bind(run_id)
        .fetch_one(database.pool())
        .await?;
    if !exists {
        return Err(ExecutionError::RunNotFound(run_id));
    }
    Ok(())
}
