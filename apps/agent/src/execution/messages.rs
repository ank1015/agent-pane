use std::collections::{HashMap, HashSet};

use llm_contracts::{AssistantContent, Message, ToolCallId, Validate as _};
use sqlx::{Postgres, Transaction, types::Json};
use uuid::Uuid;

use super::{
    AppendSessionMessages, ExecutionError, ExecutionPolicy, HarnessMessageListQuery,
    QueueRunMessage, QueuedMessageRow, QueuedRunMessage, QueuedRunMessageListQuery,
    QueuedRunMessagePage, SessionMessage, SessionMessagesAppended, constraint,
    records::{COMMITTED_MESSAGE_COLUMNS, CommittedMessageRow, QUEUED_MESSAGE_COLUMNS},
    runs,
};
use crate::db::Database;

pub(super) async fn queue(
    database: &Database,
    run_id: Uuid,
    request: QueueRunMessage,
) -> Result<super::CreateOutcome<QueuedRunMessage>, ExecutionError> {
    request.validate()?;
    let mut tx = database.pool().begin().await?;
    let context = runs::load_context(&mut tx, run_id, true).await?;
    let actual = u64::try_from(context.state_version)
        .map_err(|e| ExecutionError::InvalidStoredData(format!("runs.state_version: {e}")))?;
    if request.expected_state_version != actual {
        return Err(ExecutionError::RunStateConflict {
            expected: request.expected_state_version,
            actual,
        });
    }
    let status = agent_contracts::RunStatus::from_db(&context.status).ok_or_else(|| {
        ExecutionError::InvalidStoredData(format!("runs.status: {}", context.status))
    })?;
    if !matches!(
        status,
        agent_contracts::RunStatus::Active | agent_contracts::RunStatus::Waiting
    ) {
        return Err(ExecutionError::RunNotActive { run_id, status });
    }
    if let Some(row) = sqlx::query_as::<_, QueuedMessageRow>(&format!(
        "select {QUEUED_MESSAGE_COLUMNS} from session_messages where session_message_id = $1"
    ))
    .bind(request.input.session_message_id)
    .fetch_optional(&mut *tx)
    .await?
    {
        let existing = QueuedRunMessage::try_from(row)?;
        if existing.run_id != run_id || existing.message != request.input.message {
            return Err(ExecutionError::MessageIdConflict(
                request.input.session_message_id,
            ));
        }
        tx.commit().await?;
        return Ok(super::CreateOutcome {
            value: existing,
            created: false,
        });
    }
    let next_sequence = sqlx::query_scalar::<_, i64>("select coalesce(max(queue_sequence), 0) + 1 from session_messages where run_id = $1 and delivery = 'next_turn'")
        .bind(run_id).fetch_one(&mut *tx).await?;
    let row = sqlx::query_as::<_, QueuedMessageRow>(&format!(
        "insert into session_messages (session_message_id, session_id, message, origin, delivery, state, run_id, queued_during_turn, queue_sequence) \
         values ($1, $2, $3, 'external', 'next_turn', 'pending', $4, $5, $6) returning {QUEUED_MESSAGE_COLUMNS}"
    )).bind(request.input.session_message_id).bind(context.session_id).bind(Json(&request.input.message)).bind(run_id).bind(context.current_turn).bind(next_sequence)
      .fetch_one(&mut *tx).await.map_err(|error| if constraint(&error) == Some("session_messages_pkey") { ExecutionError::MessageIdConflict(request.input.session_message_id) } else { error.into() })?;
    let value = QueuedRunMessage::try_from(row)?;
    tx.commit().await?;
    Ok(super::CreateOutcome {
        value,
        created: true,
    })
}

pub(super) async fn list_queued(
    database: &Database,
    run_id: Uuid,
    query: QueuedRunMessageListQuery,
) -> Result<QueuedRunMessagePage, ExecutionError> {
    runs::get(database, run_id).await?;
    let limit = query.limit.unwrap_or(50);
    if !(1..=100).contains(&limit) {
        return Err(ExecutionError::InvalidQueuedMessagePageSize);
    }
    let after = i64::try_from(query.after_sequence.unwrap_or(0))
        .map_err(|_| ExecutionError::InvalidQueuedMessageAfterSequence)?;
    let mut sql = format!(
        "select {QUEUED_MESSAGE_COLUMNS} from session_messages where run_id = $1 and delivery = 'next_turn' and queue_sequence > $2"
    );
    if query.state.is_some() {
        sql.push_str(" and state = $4");
    }
    sql.push_str(" order by queue_sequence limit $3");
    let mut statement = sqlx::query_as::<_, QueuedMessageRow>(&sql)
        .bind(run_id)
        .bind(after)
        .bind(i64::from(limit) + 1);
    if let Some(state) = query.state {
        statement = statement.bind(state.as_db());
    }
    let rows = statement.fetch_all(database.pool()).await?;
    let more = rows.len() > limit as usize;
    let items = rows
        .into_iter()
        .take(limit as usize)
        .map(QueuedRunMessage::try_from)
        .collect::<Result<Vec<_>, _>>()?;
    let next_after_sequence = more
        .then(|| items.last().map(|m| m.queue_sequence))
        .flatten();
    Ok(QueuedRunMessagePage {
        items,
        next_after_sequence,
    })
}

pub(super) async fn get_queued(
    database: &Database,
    run_id: Uuid,
    message_id: Uuid,
) -> Result<QueuedRunMessage, ExecutionError> {
    let row = sqlx::query_as::<_, QueuedMessageRow>(&format!("select {QUEUED_MESSAGE_COLUMNS} from session_messages where run_id = $1 and session_message_id = $2 and delivery = 'next_turn'"))
        .bind(run_id).bind(message_id).fetch_optional(database.pool()).await?.ok_or(ExecutionError::QueuedMessageNotFound(message_id))?;
    row.try_into()
}

pub(super) async fn list_harness(
    database: &Database,
    run_id: Uuid,
    query: HarnessMessageListQuery,
) -> Result<agent_contracts::SessionMessagePage, ExecutionError> {
    let run = runs::get(database, run_id).await?;
    let limit = query.limit.unwrap_or(100);
    if !(1..=500).contains(&limit) {
        return Err(ExecutionError::InvalidMessagePageSize);
    }
    let after = i64::try_from(query.after_revision.unwrap_or(0))
        .map_err(|_| ExecutionError::InvalidMessagePageSize)?;
    let rows = sqlx::query_as::<_, CommittedMessageRow>(&format!("select {COMMITTED_MESSAGE_COLUMNS} from session_messages where session_id = $1 and state = 'committed' and revision > $2 order by revision limit $3"))
        .bind(run.session_id).bind(after).bind(i64::from(limit) + 1).fetch_all(database.pool()).await?;
    let more = rows.len() > limit as usize;
    let items = rows
        .into_iter()
        .take(limit as usize)
        .map(SessionMessage::try_from)
        .collect::<Result<Vec<_>, _>>()?;
    let next_after_revision = more.then(|| items.last().map(|m| m.revision)).flatten();
    Ok(agent_contracts::SessionMessagePage {
        items,
        next_after_revision,
    })
}

pub(super) async fn append_harness(
    database: &Database,
    policy: ExecutionPolicy,
    run_id: Uuid,
    request: AppendSessionMessages,
) -> Result<SessionMessagesAppended, ExecutionError> {
    request.validate()?;
    if request.messages.len() > policy.max_message_batch() {
        return Err(ExecutionError::MessageBatchTooLarge(
            policy.max_message_batch(),
        ));
    }
    let mut tx = database.pool().begin().await?;
    let context = runs::load_context(&mut tx, run_id, true).await?;

    let ids = request
        .messages
        .iter()
        .map(|m| m.session_message_id)
        .collect::<Vec<_>>();
    let existing = sqlx::query_as::<_, CommittedMessageRow>(&format!("select {COMMITTED_MESSAGE_COLUMNS} from session_messages where session_message_id = any($1) and state = 'committed' order by revision"))
        .bind(&ids).fetch_all(&mut *tx).await?;
    if !existing.is_empty() {
        if existing.len() != request.messages.len() {
            return Err(ExecutionError::MessageIdConflict(
                existing[0].session_message_id,
            ));
        }
        let items = existing
            .into_iter()
            .map(SessionMessage::try_from)
            .collect::<Result<Vec<_>, _>>()?;
        let valid = request.messages.iter().all(|wanted| {
            items.iter().any(|got| {
                got.session_message_id == wanted.session_message_id
                    && got.message == wanted.message
                    && got.run_id == Some(run_id)
                    && got.turn_number == Some(request.turn_number)
            })
        });
        if !valid {
            return Err(ExecutionError::MessageIdConflict(
                request.messages[0].session_message_id,
            ));
        }
        let current = u64::try_from(context.current_session_revision).map_err(|e| {
            ExecutionError::InvalidStoredData(format!("sessions.current_revision: {e}"))
        })?;
        tx.commit().await?;
        return Ok(SessionMessagesAppended {
            items,
            current_session_revision: current,
        });
    }

    if request.after_cancellation {
        require_cancelled_tool_result_append(&mut tx, &context, run_id, &request).await?;
    } else {
        runs::require_active(
            &context,
            request.expected_state_version,
            request.turn_number,
        )?;
    }
    let actual_revision = u64::try_from(context.current_session_revision).map_err(|e| {
        ExecutionError::InvalidStoredData(format!("sessions.current_revision: {e}"))
    })?;
    if actual_revision != request.expected_session_revision {
        return Err(ExecutionError::SessionRevisionConflict {
            expected: request.expected_session_revision,
            actual: actual_revision,
        });
    }
    let mut revision = context.current_session_revision;
    let mut items = Vec::with_capacity(request.messages.len());
    for message in request.messages {
        revision = revision.checked_add(1).ok_or_else(|| {
            ExecutionError::InvalidStoredData("session revision overflow".to_owned())
        })?;
        let row = sqlx::query_as::<_, CommittedMessageRow>(&format!("insert into session_messages (session_message_id, session_id, revision, message, origin, delivery, state, run_id, turn_number, committed_at) values ($1,$2,$3,$4,'harness','immediate','committed',$5,$6,now()) returning {COMMITTED_MESSAGE_COLUMNS}"))
            .bind(message.session_message_id).bind(context.session_id).bind(revision).bind(Json(&message.message)).bind(run_id).bind(context.current_turn)
            .fetch_one(&mut *tx).await.map_err(|error| if constraint(&error) == Some("session_messages_pkey") { ExecutionError::MessageIdConflict(message.session_message_id) } else { error.into() })?;
        items.push(SessionMessage::try_from(row)?);
    }
    sqlx::query("update sessions set current_revision = $2 where session_id = $1")
        .bind(context.session_id)
        .bind(revision)
        .execute(&mut *tx)
        .await?;
    tx.commit().await?;
    Ok(SessionMessagesAppended {
        items,
        current_session_revision: u64::try_from(revision).map_err(|e| {
            ExecutionError::InvalidStoredData(format!("sessions.current_revision: {e}"))
        })?,
    })
}

async fn require_cancelled_tool_result_append(
    tx: &mut Transaction<'_, Postgres>,
    context: &crate::execution::RunContextRow,
    run_id: Uuid,
    request: &AppendSessionMessages,
) -> Result<(), ExecutionError> {
    let status = agent_contracts::RunStatus::from_db(&context.status).ok_or_else(|| {
        ExecutionError::InvalidStoredData(format!("runs.status: {}", context.status))
    })?;
    if status != agent_contracts::RunStatus::Aborted {
        return Err(ExecutionError::InvalidCancellationAppend(format!(
            "run {run_id} is {status:?}, not aborted"
        )));
    }

    let actual_state_version = u64::try_from(context.state_version).map_err(|error| {
        ExecutionError::InvalidStoredData(format!("runs.state_version: {error}"))
    })?;
    let cancellation_state_version =
        request
            .expected_state_version
            .checked_add(1)
            .ok_or_else(|| {
                ExecutionError::InvalidCancellationAppend(
                    "expected state version cannot advance to the abort version".to_owned(),
                )
            })?;
    if actual_state_version != cancellation_state_version {
        return Err(ExecutionError::RunStateConflict {
            expected: cancellation_state_version,
            actual: actual_state_version,
        });
    }

    let actual_turn = u32::try_from(context.current_turn).map_err(|error| {
        ExecutionError::InvalidStoredData(format!("runs.current_turn: {error}"))
    })?;
    if actual_turn != request.turn_number {
        return Err(ExecutionError::RunTurnConflict {
            expected: request.turn_number,
            actual: actual_turn,
        });
    }
    let abort_turn =
        sqlx::query_scalar::<_, i32>("select turn_number from run_aborts where run_id = $1")
            .bind(run_id)
            .fetch_optional(&mut **tx)
            .await?
            .ok_or_else(|| {
                ExecutionError::InvalidCancellationAppend(
                    "the aborted run has no matching abort record".to_owned(),
                )
            })?;
    if abort_turn != context.current_turn {
        return Err(ExecutionError::InvalidCancellationAppend(format!(
            "abort belongs to turn {abort_turn}, not {}",
            context.current_turn
        )));
    }

    let rows = sqlx::query_as::<_, CommittedMessageRow>(&format!(
        "select {COMMITTED_MESSAGE_COLUMNS} from session_messages where run_id = $1 and turn_number = $2 and state = 'committed' order by revision"
    ))
    .bind(run_id)
    .bind(context.current_turn)
    .fetch_all(&mut **tx)
    .await?;
    let mut calls = HashMap::<ToolCallId, String>::new();
    let mut completed = HashSet::<ToolCallId>::new();
    for row in rows {
        match row.message.0 {
            Message::Assistant(assistant) => {
                for content in assistant.content {
                    if let AssistantContent::ToolCall {
                        name, tool_call_id, ..
                    } = content
                        && calls.insert(tool_call_id.clone(), name).is_some()
                    {
                        return Err(ExecutionError::InvalidCancellationAppend(format!(
                            "tool call {tool_call_id:?} is ambiguous in the aborted turn"
                        )));
                    }
                }
            }
            Message::ToolResult(result) => {
                completed.insert(result.tool_call_id);
            }
            _ => {}
        }
    }

    let mut requested = HashSet::new();
    for message in &request.messages {
        let Message::ToolResult(result) = &message.message else {
            return Err(ExecutionError::InvalidCancellationAppend(
                "only tool results may be appended after cancellation".to_owned(),
            ));
        };
        let Some(expected_name) = calls.get(&result.tool_call_id) else {
            return Err(ExecutionError::InvalidCancellationAppend(format!(
                "tool result references unknown call {:?}",
                result.tool_call_id
            )));
        };
        if expected_name != &result.tool_name {
            return Err(ExecutionError::InvalidCancellationAppend(format!(
                "tool result for {:?} names {:?}, expected {:?}",
                result.tool_call_id, result.tool_name, expected_name
            )));
        }
        if completed.contains(&result.tool_call_id)
            || !requested.insert(result.tool_call_id.clone())
        {
            return Err(ExecutionError::InvalidCancellationAppend(format!(
                "tool call {:?} already has a result",
                result.tool_call_id
            )));
        }
    }
    Ok(())
}
