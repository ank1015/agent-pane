use llm_contracts::Validate as _;
use sqlx::{Postgres, Transaction, types::Json};
use uuid::Uuid;

use super::{
    ExecutionError, ExecutionPolicy, Run, RunAccepted, RunLimits, StartRun, configuration,
    policy::ResolvedRunLimits,
    records::{
        COMMITTED_MESSAGE_COLUMNS, CommittedMessageRow, RUN_COLUMNS, RunRow, SessionStateRow,
    },
};
use crate::db::Database;

pub(super) struct StartOutcome {
    pub accepted: RunAccepted,
    pub created: bool,
}

pub(super) async fn start(
    database: &Database,
    policy: ExecutionPolicy,
    session_id: Uuid,
    command: StartRun,
) -> Result<StartOutcome, ExecutionError> {
    command.validate()?;
    let limits = policy.resolve(command.limits)?;
    let mut transaction = database.pool().begin().await?;

    if let Some(run) = find_run(&mut transaction, command.run_id, true).await? {
        let accepted = existing(&mut transaction, session_id, &command, run).await?;
        transaction.commit().await?;
        return Ok(StartOutcome {
            accepted,
            created: false,
        });
    }

    let session = lock_session(&mut transaction, session_id).await?;
    if let Some(run) = find_run(&mut transaction, command.run_id, true).await? {
        let accepted = existing(&mut transaction, session_id, &command, run).await?;
        transaction.commit().await?;
        return Ok(StartOutcome {
            accepted,
            created: false,
        });
    }
    let current_revision = non_negative_revision(session.current_revision)?;
    if let Some(expected) = command.expected_session_revision
        && expected != current_revision
    {
        return Err(ExecutionError::SessionRevisionConflict {
            expected,
            actual: current_revision,
        });
    }
    if has_active_run(&mut transaction, session_id).await? {
        return Err(ExecutionError::SessionHasActiveRun(session_id));
    }

    let configuration = configuration::resolve_for_start(
        &mut transaction,
        &command.harness,
        &command.config_override,
    )
    .await?;
    let next_revision = session
        .current_revision
        .checked_add(1)
        .ok_or_else(|| ExecutionError::InvalidStoredData("session revision overflow".to_owned()))?;
    let trigger_message =
        insert_trigger_message(&mut transaction, session_id, next_revision, &command).await?;
    let run = insert_run(
        &mut transaction,
        session_id,
        &command,
        &configuration.harness_revision_id,
        &configuration.value,
        limits,
    )
    .await?;
    sqlx::query("update sessions set current_revision = $2 where session_id = $1")
        .bind(session_id)
        .bind(next_revision)
        .execute(&mut *transaction)
        .await?;
    transaction.commit().await?;

    Ok(StartOutcome {
        accepted: RunAccepted {
            trigger_message,
            run,
        },
        created: true,
    })
}

pub(super) async fn get(database: &Database, run_id: Uuid) -> Result<Run, ExecutionError> {
    let mut transaction = database.pool().begin().await?;
    let run = find_run(&mut transaction, run_id, false)
        .await?
        .ok_or(ExecutionError::RunNotFound(run_id))?;
    transaction.commit().await?;
    run.try_into()
}

async fn existing(
    transaction: &mut Transaction<'_, Postgres>,
    session_id: Uuid,
    command: &StartRun,
    run: RunRow,
) -> Result<RunAccepted, ExecutionError> {
    let configuration = configuration::resolve_existing(
        transaction,
        &run.harness_revision_id,
        &command.config_override,
    )
    .await?;
    let trigger_message = find_message(transaction, run.trigger_message_id)
        .await?
        .ok_or_else(|| {
            ExecutionError::InvalidStoredData(format!(
                "run {} references missing trigger message {}",
                run.run_id, run.trigger_message_id
            ))
        })?;
    let same_limits = requested_limits_match(command.limits, &run)?;
    let same_input = trigger_message.session_message_id == command.input.session_message_id
        && trigger_message.message.0 == command.input.message;
    if run.session_id != session_id
        || !same_input
        || !same_limits
        || !configuration::selection_matches(&command.harness, &configuration)
        || run.resolved_config.0 != configuration.value
    {
        return Err(ExecutionError::RunIdConflict(command.run_id));
    }

    Ok(RunAccepted {
        trigger_message: trigger_message.try_into()?,
        run: run.try_into()?,
    })
}

fn requested_limits_match(requested: RunLimits, run: &RunRow) -> Result<bool, ExecutionError> {
    let max_turns =
        u32::try_from(run.max_turns).map_err(|error| invalid("runs.max_turns", error))?;
    let max_failures_per_turn = u32::try_from(run.max_failures_per_turn)
        .map_err(|error| invalid("runs.max_failures_per_turn", error))?;
    Ok(requested.max_turns.is_none_or(|value| value == max_turns)
        && requested
            .max_failures_per_turn
            .is_none_or(|value| value == max_failures_per_turn))
}

async fn lock_session(
    transaction: &mut Transaction<'_, Postgres>,
    session_id: Uuid,
) -> Result<SessionStateRow, ExecutionError> {
    sqlx::query_as::<_, SessionStateRow>(
        "select current_revision from sessions where session_id = $1 for update",
    )
    .bind(session_id)
    .fetch_optional(&mut **transaction)
    .await?
    .ok_or(ExecutionError::SessionNotFound(session_id))
}

async fn has_active_run(
    transaction: &mut Transaction<'_, Postgres>,
    session_id: Uuid,
) -> Result<bool, ExecutionError> {
    sqlx::query_scalar(
        "select exists( \
             select 1 from runs where session_id = $1 \
             and status in ('queued', 'running', 'waiting', 'aborting') \
         )",
    )
    .bind(session_id)
    .fetch_one(&mut **transaction)
    .await
    .map_err(Into::into)
}

async fn insert_trigger_message(
    transaction: &mut Transaction<'_, Postgres>,
    session_id: Uuid,
    revision: i64,
    command: &StartRun,
) -> Result<crate::sessions::SessionMessage, ExecutionError> {
    let result = sqlx::query_as::<_, CommittedMessageRow>(&format!(
        "insert into session_messages \
         (session_message_id, session_id, revision, message, origin, delivery, state, committed_at) \
         values ($1, $2, $3, $4, 'external', 'immediate', 'committed', now()) \
         returning {COMMITTED_MESSAGE_COLUMNS}"
    ))
    .bind(command.input.session_message_id)
    .bind(session_id)
    .bind(revision)
    .bind(Json(&command.input.message))
    .fetch_one(&mut **transaction)
    .await;
    match result {
        Ok(row) => row.try_into(),
        Err(error) if super::constraint(&error) == Some("session_messages_pkey") => Err(
            ExecutionError::MessageIdConflict(command.input.session_message_id),
        ),
        Err(error) => Err(error.into()),
    }
}

async fn insert_run(
    transaction: &mut Transaction<'_, Postgres>,
    session_id: Uuid,
    command: &StartRun,
    harness_revision_id: &str,
    resolved_config: &llm_contracts::JsonObject,
    limits: ResolvedRunLimits,
) -> Result<Run, ExecutionError> {
    let result = sqlx::query_as::<_, RunRow>(&format!(
        "insert into runs \
         (run_id, session_id, trigger_message_id, harness_revision_id, resolved_config, \
          max_turns, max_failures_per_turn) \
         values ($1, $2, $3, $4, $5, $6, $7) returning {RUN_COLUMNS}"
    ))
    .bind(command.run_id)
    .bind(session_id)
    .bind(command.input.session_message_id)
    .bind(harness_revision_id)
    .bind(Json(resolved_config))
    .bind(i32::try_from(limits.max_turns).map_err(|error| invalid("max_turns", error))?)
    .bind(
        i32::try_from(limits.max_failures_per_turn)
            .map_err(|error| invalid("max_failures_per_turn", error))?,
    )
    .fetch_one(&mut **transaction)
    .await;
    match result {
        Ok(row) => row.try_into(),
        Err(error) if super::constraint(&error) == Some("runs_pkey") => {
            Err(ExecutionError::RunIdConflict(command.run_id))
        }
        Err(error) if super::constraint(&error) == Some("runs_one_active_per_session_idx") => {
            Err(ExecutionError::SessionHasActiveRun(session_id))
        }
        Err(error) if super::constraint(&error) == Some("runs_trigger_message_unique") => Err(
            ExecutionError::MessageIdConflict(command.input.session_message_id),
        ),
        Err(error) => Err(error.into()),
    }
}

async fn find_run(
    transaction: &mut Transaction<'_, Postgres>,
    run_id: Uuid,
    lock: bool,
) -> Result<Option<RunRow>, ExecutionError> {
    let suffix = if lock { " for share" } else { "" };
    let query = format!("select {RUN_COLUMNS} from runs where run_id = $1{suffix}");
    sqlx::query_as::<_, RunRow>(&query)
        .bind(run_id)
        .fetch_optional(&mut **transaction)
        .await
        .map_err(Into::into)
}

async fn find_message(
    transaction: &mut Transaction<'_, Postgres>,
    message_id: Uuid,
) -> Result<Option<CommittedMessageRow>, ExecutionError> {
    let query = format!(
        "select {COMMITTED_MESSAGE_COLUMNS} from session_messages \
         where session_message_id = $1 and state = 'committed'"
    );
    sqlx::query_as::<_, CommittedMessageRow>(&query)
        .bind(message_id)
        .fetch_optional(&mut **transaction)
        .await
        .map_err(Into::into)
}

fn non_negative_revision(value: i64) -> Result<u64, ExecutionError> {
    u64::try_from(value).map_err(|error| invalid("sessions.current_revision", error))
}

fn invalid(field: &str, error: impl std::fmt::Display) -> ExecutionError {
    ExecutionError::InvalidStoredData(format!("{field}: {error}"))
}
