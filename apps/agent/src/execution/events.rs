use agent_contracts::{
    HarnessRunEvent, HarnessRunEventData, RunEvent, RunEventData, RunEventListQuery, RunEventPage,
    RunEventSource, RunStatus,
};
use chrono::{DateTime, Utc};
use llm_contracts::Validate as _;
use sqlx::{FromRow, Postgres, Transaction, types::Json};
use uuid::Uuid;

use super::{ExecutionError, records::positive_u64, runs};
use crate::db::{Database, RUN_EVENT_NOTIFICATION_CHANNEL};

const EVENT_COLUMNS: &str = "event_id, run_id, sequence, turn_number, state_version, run_status, source, payload, occurred_at, recorded_at";

#[derive(FromRow)]
struct RunEventRow {
    event_id: Uuid,
    run_id: Uuid,
    sequence: i64,
    turn_number: Option<i32>,
    state_version: i64,
    run_status: String,
    source: String,
    payload: Json<RunEventData>,
    occurred_at: DateTime<Utc>,
    recorded_at: DateTime<Utc>,
}

impl TryFrom<RunEventRow> for RunEvent {
    type Error = ExecutionError;

    fn try_from(row: RunEventRow) -> Result<Self, Self::Error> {
        Ok(Self {
            event_id: row.event_id,
            run_id: row.run_id,
            sequence: positive_u64("run_events.sequence", row.sequence)?,
            turn_number: row
                .turn_number
                .map(|value| {
                    u32::try_from(value).map_err(|error| {
                        ExecutionError::InvalidStoredData(format!(
                            "run_events.turn_number: {error}"
                        ))
                    })
                })
                .transpose()?,
            state_version: positive_u64("run_events.state_version", row.state_version)?,
            run_status: RunStatus::from_db(&row.run_status).ok_or_else(|| {
                ExecutionError::InvalidStoredData(format!(
                    "run_events.run_status: {}",
                    row.run_status
                ))
            })?,
            source: RunEventSource::from_db(&row.source).ok_or_else(|| {
                ExecutionError::InvalidStoredData(format!("run_events.source: {}", row.source))
            })?,
            occurred_at: row.occurred_at,
            recorded_at: row.recorded_at,
            event: row.payload.0,
        })
    }
}

#[allow(clippy::too_many_arguments)]
pub(super) async fn append_agent(
    tx: &mut Transaction<'_, Postgres>,
    run_id: Uuid,
    turn_number: Option<u32>,
    state_version: u64,
    run_status: RunStatus,
    event_id: Uuid,
    occurred_at: DateTime<Utc>,
    event: RunEventData,
) -> Result<RunEvent, ExecutionError> {
    append(
        tx,
        run_id,
        turn_number,
        state_version,
        run_status,
        RunEventSource::Agent,
        event_id,
        occurred_at,
        event,
    )
    .await
}

#[allow(clippy::too_many_arguments)]
async fn append(
    tx: &mut Transaction<'_, Postgres>,
    run_id: Uuid,
    turn_number: Option<u32>,
    state_version: u64,
    run_status: RunStatus,
    source: RunEventSource,
    event_id: Uuid,
    occurred_at: DateTime<Utc>,
    event: RunEventData,
) -> Result<RunEvent, ExecutionError> {
    let sequence = sqlx::query_scalar::<_, i64>(
        "update runs set last_event_sequence = last_event_sequence + 1 where run_id = $1 returning last_event_sequence",
    )
    .bind(run_id)
    .fetch_one(&mut **tx)
    .await?;
    let row = sqlx::query_as::<_, RunEventRow>(&format!(
        "insert into run_events (event_id, run_id, sequence, turn_number, state_version, run_status, source, payload, occurred_at) values ($1,$2,$3,$4,$5,$6,$7,$8,$9) returning {EVENT_COLUMNS}"
    ))
    .bind(event_id)
    .bind(run_id)
    .bind(sequence)
    .bind(turn_number.map(i32::try_from).transpose().map_err(|error| {
        ExecutionError::InvalidStoredData(format!("run event turn number: {error}"))
    })?)
    .bind(i64::try_from(state_version).map_err(|error| {
        ExecutionError::InvalidStoredData(format!("run event state version: {error}"))
    })?)
    .bind(run_status.as_db())
    .bind(source.as_db())
    .bind(Json(event))
    .bind(occurred_at)
    .fetch_one(&mut **tx)
    .await?;
    // PostgreSQL only delivers this after the surrounding transaction commits.
    // It is a wake-up hint; the durable event row remains the source of truth.
    sqlx::query("select pg_notify($1, $2)")
        .bind(RUN_EVENT_NOTIFICATION_CHANNEL)
        .bind(run_id.to_string())
        .execute(&mut **tx)
        .await?;
    row.try_into()
}

pub(super) async fn list(
    database: &Database,
    run_id: Uuid,
    query: RunEventListQuery,
) -> Result<RunEventPage, ExecutionError> {
    runs::get(database, run_id).await?;
    let limit = query.limit.unwrap_or(100);
    if !(1..=500).contains(&limit) {
        return Err(ExecutionError::InvalidRunEventPageSize);
    }
    let after = i64::try_from(query.after_sequence.unwrap_or(0))
        .map_err(|_| ExecutionError::InvalidRunEventAfterSequence)?;
    let rows = sqlx::query_as::<_, RunEventRow>(&format!(
        "select {EVENT_COLUMNS} from run_events where run_id = $1 and sequence > $2 order by sequence limit $3"
    ))
    .bind(run_id)
    .bind(after)
    .bind(i64::from(limit) + 1)
    .fetch_all(database.pool())
    .await?;
    let more = rows.len() > limit as usize;
    let items = rows
        .into_iter()
        .take(limit as usize)
        .map(RunEvent::try_from)
        .collect::<Result<Vec<_>, _>>()?;
    let next_after_sequence = more
        .then(|| items.last().map(|event| event.sequence))
        .flatten();
    Ok(RunEventPage {
        items,
        next_after_sequence,
    })
}

pub async fn ingest_harness_event(
    database: &Database,
    event: &HarnessRunEvent,
) -> Result<HarnessEventIngestOutcome, ExecutionError> {
    event.validate()?;
    let mut tx = database.pool().begin().await?;
    let context = match runs::load_context(&mut tx, event.run_id, true).await {
        Ok(context) => context,
        Err(ExecutionError::RunNotFound(_)) => {
            tx.rollback().await?;
            return Ok(HarnessEventIngestOutcome::Rejected);
        }
        Err(error) => return Err(error),
    };
    let duplicate = sqlx::query_scalar::<_, bool>(
        "select exists(select 1 from run_events where event_id = $1)",
    )
    .bind(event.event_id)
    .fetch_one(&mut *tx)
    .await?;
    if duplicate {
        tx.commit().await?;
        return Ok(HarnessEventIngestOutcome::Duplicate);
    }
    if context.harness_slug != event.harness_slug
        || runs::require_active(&context, event.expected_state_version, event.turn_number).is_err()
    {
        tx.commit().await?;
        return Ok(HarnessEventIngestOutcome::Rejected);
    }
    let status = RunStatus::from_db(&context.status).ok_or_else(|| {
        ExecutionError::InvalidStoredData(format!("runs.status: {}", context.status))
    })?;
    let data = match &event.event {
        HarnessRunEventData::TurnStarted => RunEventData::TurnStarted,
        HarnessRunEventData::Progress { name, data } => RunEventData::Progress {
            name: name.clone(),
            data: data.clone(),
        },
    };
    append(
        &mut tx,
        event.run_id,
        Some(event.turn_number),
        event.expected_state_version,
        status,
        RunEventSource::Harness,
        event.event_id,
        event.emitted_at,
        data,
    )
    .await?;
    tx.commit().await?;
    Ok(HarnessEventIngestOutcome::Applied)
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum HarnessEventIngestOutcome {
    Applied,
    Duplicate,
    Rejected,
}
