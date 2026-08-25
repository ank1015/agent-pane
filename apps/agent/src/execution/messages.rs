use std::collections::HashMap;

use llm_contracts::Validate as _;
use sqlx::types::Json;
use uuid::Uuid;

use super::{
    AppendRunMessages, ExecutionError, ExecutionPolicy, RunMessagesAppended, SessionMessage,
    leasing::{LeaseToken, lock_lease, lock_run, require_running_or_aborting, verify_lease},
    records::{COMMITTED_MESSAGE_COLUMNS, CommittedMessageRow},
};
use crate::{db::Database, sessions::SessionMessagePage};

const DEFAULT_PAGE_SIZE: u32 = 100;
const MAX_PAGE_SIZE: u32 = 500;

pub(super) async fn list(
    database: &Database,
    run_id: Uuid,
    lease_version: u64,
    token: &LeaseToken,
    after_revision: Option<u64>,
    limit: Option<u32>,
) -> Result<SessionMessagePage, ExecutionError> {
    if lease_version == 0 {
        return Err(llm_contracts::ValidationError::single(
            "lease_version",
            "must be greater than zero",
        )
        .into());
    }
    let limit = limit.unwrap_or(DEFAULT_PAGE_SIZE);
    if !(1..=MAX_PAGE_SIZE).contains(&limit) {
        return Err(ExecutionError::InvalidMessagePageSize);
    }
    let after_revision = after_revision.unwrap_or(0);
    let after_revision =
        i64::try_from(after_revision).map_err(|_| ExecutionError::InvalidMessagePageSize)?;

    let mut transaction = database.pool().begin().await?;
    let run = lock_run(&mut transaction, run_id).await?;
    require_running_or_aborting(&run)?;
    let lease = lock_lease(&mut transaction, run_id).await?;
    verify_lease(&lease, lease_version, token)?;
    let query = format!(
        "select {COMMITTED_MESSAGE_COLUMNS} from session_messages \
         where session_id = $1 and state = 'committed' and revision > $2 \
         order by revision limit $3"
    );
    let mut rows = sqlx::query_as::<_, CommittedMessageRow>(&query)
        .bind(run.session_id)
        .bind(after_revision)
        .bind(i64::from(limit) + 1)
        .fetch_all(&mut *transaction)
        .await?;
    let has_more = rows.len() > limit as usize;
    if has_more {
        rows.pop();
    }
    let items = rows
        .into_iter()
        .map(SessionMessage::try_from)
        .collect::<Result<Vec<_>, _>>()?;
    let next_after_revision = has_more.then(|| {
        items
            .last()
            .expect("a page with more results is not empty")
            .revision
    });
    transaction.commit().await?;
    Ok(SessionMessagePage {
        items,
        next_after_revision,
    })
}

pub(super) async fn append(
    database: &Database,
    policy: ExecutionPolicy,
    run_id: Uuid,
    command: AppendRunMessages,
    token: &LeaseToken,
) -> Result<RunMessagesAppended, ExecutionError> {
    command.validate()?;
    if command.messages.len() > policy.max_message_batch() {
        return Err(ExecutionError::MessageBatchTooLarge(
            policy.max_message_batch(),
        ));
    }

    let mut transaction = database.pool().begin().await?;
    let run = lock_run(&mut transaction, run_id).await?;
    require_running_or_aborting(&run)?;
    let lease = lock_lease(&mut transaction, run_id).await?;
    verify_lease(&lease, command.lease_version, token)?;
    let current_revision = sqlx::query_scalar::<_, i64>(
        "select current_revision from sessions where session_id = $1 for update",
    )
    .bind(run.session_id)
    .fetch_one(&mut *transaction)
    .await?;

    let ids = command
        .messages
        .iter()
        .map(|message| message.session_message_id)
        .collect::<Vec<_>>();
    let query = format!(
        "select {COMMITTED_MESSAGE_COLUMNS} from session_messages \
         where session_message_id = any($1::uuid[]) and state = 'committed'"
    );
    let existing = sqlx::query_as::<_, CommittedMessageRow>(&query)
        .bind(&ids)
        .fetch_all(&mut *transaction)
        .await?;
    if !existing.is_empty() {
        if existing.len() != command.messages.len() {
            return Err(ExecutionError::MessageIdConflict(
                existing[0].session_message_id,
            ));
        }
        let mut existing = existing
            .into_iter()
            .map(|row| Ok((row.session_message_id, SessionMessage::try_from(row)?)))
            .collect::<Result<HashMap<_, _>, ExecutionError>>()?;
        let mut items = Vec::with_capacity(command.messages.len());
        for requested in &command.messages {
            let stored = existing.remove(&requested.session_message_id).ok_or(
                ExecutionError::MessageIdConflict(requested.session_message_id),
            )?;
            if stored.session_id != run.session_id
                || stored.run_id != Some(run_id)
                || stored.turn_number
                    != Some(u32::try_from(run.current_turn).map_err(|error| {
                        ExecutionError::InvalidStoredData(format!("runs.current_turn: {error}"))
                    })?)
                || stored.origin != crate::sessions::SessionMessageOrigin::Harness
                || stored.delivery != crate::sessions::SessionMessageDelivery::Immediate
                || stored.message != requested.message
            {
                return Err(ExecutionError::MessageIdConflict(
                    requested.session_message_id,
                ));
            }
            items.push(stored);
        }
        let current_session_revision = u64::try_from(current_revision).map_err(|error| {
            ExecutionError::InvalidStoredData(format!("sessions.current_revision: {error}"))
        })?;
        transaction.commit().await?;
        return Ok(RunMessagesAppended {
            items,
            current_session_revision,
        });
    }

    let expected = i64::try_from(command.expected_session_revision).map_err(|_| {
        ExecutionError::SessionRevisionConflict {
            expected: command.expected_session_revision,
            actual: u64::try_from(current_revision).unwrap_or(0),
        }
    })?;
    if expected != current_revision {
        return Err(ExecutionError::SessionRevisionConflict {
            expected: command.expected_session_revision,
            actual: u64::try_from(current_revision).map_err(|error| {
                ExecutionError::InvalidStoredData(format!("sessions.current_revision: {error}"))
            })?,
        });
    }

    let mut revision = current_revision;
    let mut items = Vec::with_capacity(command.messages.len());
    for message in command.messages {
        revision = revision.checked_add(1).ok_or_else(|| {
            ExecutionError::InvalidStoredData("session revision overflow".to_owned())
        })?;
        let query = format!(
            "insert into session_messages \
             (session_message_id, session_id, revision, message, origin, delivery, state, \
              run_id, turn_number, committed_at) \
             values ($1, $2, $3, $4, 'harness', 'immediate', 'committed', $5, $6, now()) \
             returning {COMMITTED_MESSAGE_COLUMNS}"
        );
        let row = sqlx::query_as::<_, CommittedMessageRow>(&query)
            .bind(message.session_message_id)
            .bind(run.session_id)
            .bind(revision)
            .bind(Json(message.message))
            .bind(run_id)
            .bind(run.current_turn)
            .fetch_one(&mut *transaction)
            .await
            .map_err(|error| match super::constraint(&error) {
                Some("session_messages_pkey") => {
                    ExecutionError::MessageIdConflict(message.session_message_id)
                }
                _ => ExecutionError::Database(error),
            })?;
        items.push(row.try_into()?);
    }
    sqlx::query("update sessions set current_revision = $2 where session_id = $1")
        .bind(run.session_id)
        .bind(revision)
        .execute(&mut *transaction)
        .await?;
    transaction.commit().await?;
    Ok(RunMessagesAppended {
        items,
        current_session_revision: u64::try_from(revision).map_err(|error| {
            ExecutionError::InvalidStoredData(format!("sessions.current_revision: {error}"))
        })?,
    })
}
