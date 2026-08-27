use agent_contracts::{HARNESS_PROTOCOL_VERSION, TurnRequested, turn_subject};
use chrono::Utc;
use llm_contracts::Validate as _;
use sqlx::{Postgres, Transaction, types::Json};
use uuid::Uuid;

use super::{
    CreateOutcome, ExecutionError, ExecutionPolicy, Run, RunAccepted, RunContextRow, RunRow,
    RunStatus, StartRun, configuration, constraint, outbox,
    records::{
        COMMITTED_MESSAGE_COLUMNS, CommittedMessageRow, RUN_COLUMNS, positive_u32, positive_u64,
    },
};
use crate::{db::Database, sessions::SessionMessage};

pub(super) async fn start(
    database: &Database,
    policy: ExecutionPolicy,
    session_id: Uuid,
    request: StartRun,
) -> Result<CreateOutcome<RunAccepted>, ExecutionError> {
    request.validate()?;
    let max_turns = policy.resolve(request.limits)?;
    let mut tx = database.pool().begin().await?;

    if let Some(existing) = find_run(&mut tx, request.run_id).await? {
        if existing.session_id != session_id
            || existing.trigger_message_id != request.input.session_message_id
        {
            return Err(ExecutionError::RunIdConflict(request.run_id));
        }
        let trigger = get_committed_message(&mut tx, request.input.session_message_id).await?;
        tx.commit().await?;
        return Ok(CreateOutcome {
            value: RunAccepted {
                trigger_message: trigger,
                run: existing,
            },
            created: false,
        });
    }

    let current_revision = sqlx::query_scalar::<_, i64>(
        "select current_revision from sessions where session_id = $1 for update",
    )
    .bind(session_id)
    .fetch_optional(&mut *tx)
    .await?
    .ok_or(ExecutionError::SessionNotFound(session_id))?;
    let current_revision_u64 = u64::try_from(current_revision).map_err(|e| {
        ExecutionError::InvalidStoredData(format!("sessions.current_revision: {e}"))
    })?;
    if let Some(expected) = request.expected_session_revision
        && expected != current_revision_u64
    {
        return Err(ExecutionError::SessionRevisionConflict {
            expected,
            actual: current_revision_u64,
        });
    }

    let resolved =
        configuration::resolve_for_start(&mut tx, &request.harness, &request.config_override)
            .await?;
    let revision = current_revision
        .checked_add(1)
        .ok_or_else(|| ExecutionError::InvalidStoredData("session revision overflow".to_owned()))?;
    let message_insert = sqlx::query_as::<_, CommittedMessageRow>(&format!(
        "insert into session_messages (session_message_id, session_id, revision, message, origin, delivery, state, created_at, committed_at) \
         values ($1, $2, $3, $4, 'external', 'immediate', 'committed', now(), now()) returning {COMMITTED_MESSAGE_COLUMNS}"
    ))
    .bind(request.input.session_message_id)
    .bind(session_id)
    .bind(revision)
    .bind(Json(&request.input.message))
    .fetch_one(&mut *tx)
    .await;
    let trigger = match message_insert {
        Ok(row) => SessionMessage::try_from(row)?,
        Err(error) if constraint(&error) == Some("session_messages_pkey") => {
            return Err(ExecutionError::MessageIdConflict(
                request.input.session_message_id,
            ));
        }
        Err(error) => return Err(error.into()),
    };
    sqlx::query("update sessions set current_revision = $2 where session_id = $1")
        .bind(session_id)
        .bind(revision)
        .execute(&mut *tx)
        .await?;

    let run_insert = sqlx::query_as::<_, RunRow>(&format!(
        "insert into runs (run_id, session_id, trigger_message_id, harness_revision_id, resolved_config, max_turns) \
         values ($1, $2, $3, $4, $5, $6) returning {RUN_COLUMNS}"
    ))
    .bind(request.run_id).bind(session_id).bind(request.input.session_message_id)
    .bind(&resolved.harness_revision_id).bind(Json(&resolved.value)).bind(i32::try_from(max_turns).map_err(|e| ExecutionError::InvalidStoredData(format!("max_turns: {e}")))?)
    .fetch_one(&mut *tx).await;
    let run = match run_insert {
        Ok(row) => Run::try_from(row)?,
        Err(error) if constraint(&error) == Some("runs_one_active_per_session_idx") => {
            return Err(ExecutionError::SessionHasActiveRun(session_id));
        }
        Err(error) if constraint(&error) == Some("runs_pkey") => {
            return Err(ExecutionError::RunIdConflict(request.run_id));
        }
        Err(error) => return Err(error.into()),
    };

    let context = load_context(&mut tx, request.run_id, true).await?;
    enqueue_turn(&mut tx, &context, None).await?;
    tx.commit().await?;
    Ok(CreateOutcome {
        value: RunAccepted {
            trigger_message: trigger,
            run,
        },
        created: true,
    })
}

pub(super) async fn get(database: &Database, run_id: Uuid) -> Result<Run, ExecutionError> {
    let row =
        sqlx::query_as::<_, RunRow>(&format!("select {RUN_COLUMNS} from runs where run_id = $1"))
            .bind(run_id)
            .fetch_optional(database.pool())
            .await?
            .ok_or(ExecutionError::RunNotFound(run_id))?;
    row.try_into()
}

pub(super) async fn find_run(
    tx: &mut Transaction<'_, Postgres>,
    run_id: Uuid,
) -> Result<Option<Run>, ExecutionError> {
    sqlx::query_as::<_, RunRow>(&format!("select {RUN_COLUMNS} from runs where run_id = $1"))
        .bind(run_id)
        .fetch_optional(&mut **tx)
        .await?
        .map(Run::try_from)
        .transpose()
}

pub(super) async fn get_committed_message(
    tx: &mut Transaction<'_, Postgres>,
    message_id: Uuid,
) -> Result<SessionMessage, ExecutionError> {
    let row = sqlx::query_as::<_, CommittedMessageRow>(&format!("select {COMMITTED_MESSAGE_COLUMNS} from session_messages where session_message_id = $1 and state = 'committed'"))
        .bind(message_id).fetch_optional(&mut **tx).await?.ok_or(ExecutionError::MessageIdConflict(message_id))?;
    row.try_into()
}

pub(super) async fn load_context(
    tx: &mut Transaction<'_, Postgres>,
    run_id: Uuid,
    lock: bool,
) -> Result<RunContextRow, ExecutionError> {
    let suffix = if lock { " for update of r, s" } else { "" };
    let query = format!(
        "select r.run_id, r.session_id, r.harness_revision_id, hr.harness_id, h.slug as harness_slug, \
         r.resolved_config, r.status, r.current_turn, r.max_turns, r.state_version, s.current_revision as current_session_revision \
         from runs r join sessions s on s.session_id = r.session_id \
         join harness_revisions hr on hr.harness_revision_id = r.harness_revision_id \
         join harnesses h on h.harness_id = hr.harness_id where r.run_id = $1{suffix}"
    );
    sqlx::query_as::<_, RunContextRow>(&query)
        .bind(run_id)
        .fetch_optional(&mut **tx)
        .await?
        .ok_or(ExecutionError::RunNotFound(run_id))
}

pub(super) async fn enqueue_turn(
    tx: &mut Transaction<'_, Postgres>,
    context: &RunContextRow,
    resume: Option<agent_contracts::WaitResume>,
) -> Result<(), ExecutionError> {
    let event_id = Uuid::now_v7();
    let event = TurnRequested {
        protocol_version: HARNESS_PROTOCOL_VERSION,
        event_id,
        emitted_at: Utc::now(),
        run_id: context.run_id,
        session_id: context.session_id,
        turn_number: positive_u32("runs.current_turn", context.current_turn)?,
        max_turns: positive_u32("runs.max_turns", context.max_turns)?,
        expected_state_version: positive_u64("runs.state_version", context.state_version)?,
        harness_id: context.harness_id.clone(),
        harness_slug: context.harness_slug.clone(),
        harness_revision_id: context.harness_revision_id.clone(),
        resolved_config: context.resolved_config.0.clone(),
        current_session_revision: u64::try_from(context.current_session_revision).map_err(|e| {
            ExecutionError::InvalidStoredData(format!("sessions.current_revision: {e}"))
        })?,
        resume,
    };
    outbox::enqueue(tx, event_id, &turn_subject(&context.harness_slug), &event).await
}

pub(super) fn require_active(
    context: &RunContextRow,
    expected_state_version: u64,
    turn_number: u32,
) -> Result<(), ExecutionError> {
    let status = RunStatus::from_db(&context.status).ok_or_else(|| {
        ExecutionError::InvalidStoredData(format!("runs.status: {}", context.status))
    })?;
    let actual_state_version = positive_u64("runs.state_version", context.state_version)?;
    let actual_turn = positive_u32("runs.current_turn", context.current_turn)?;
    if expected_state_version != actual_state_version {
        return Err(ExecutionError::RunStateConflict {
            expected: expected_state_version,
            actual: actual_state_version,
        });
    }
    if turn_number != actual_turn {
        return Err(ExecutionError::RunTurnConflict {
            expected: turn_number,
            actual: actual_turn,
        });
    }
    if status != RunStatus::Active {
        return Err(ExecutionError::RunNotActive {
            run_id: context.run_id,
            status,
        });
    }
    Ok(())
}
