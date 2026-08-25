use base64::{Engine as _, engine::general_purpose::URL_SAFE_NO_PAD};
use chrono::{DateTime, Utc};
use llm_contracts::{JsonObject, Validate as _};
use serde::{Deserialize, Serialize};
use sqlx::{FromRow, Postgres, QueryBuilder, Transaction, types::Json};
use uuid::Uuid;

use super::{
    ExecutionError, RequestRunWait, ResolveRunWait, Run, RunStatus, RunWait, RunWaitPage,
    RunWaitRequested, RunWaitResolved, WaitListQuery, WaitResume, WaitStatus,
    finishing::{delete_lease, require_state_version},
    leasing::{LeaseToken, lock_lease, lock_run, require_running, verify_lease},
    records::{RUN_COLUMNS, RunRow, positive_u32},
};
use crate::db::Database;

const DEFAULT_PAGE_SIZE: u32 = 50;
const MAX_PAGE_SIZE: u32 = 100;
const WAIT_COLUMNS: &str = "wait_id, run_id, turn_number, harness_wait_id, kind, public_request, \
    resume_metadata, status, resolution, requested_at, resolved_at, cancelled_at";

pub(super) struct WaitOutcome<T> {
    pub value: T,
    pub created: bool,
}

#[derive(FromRow)]
struct WaitRow {
    wait_id: Uuid,
    run_id: Uuid,
    turn_number: i32,
    harness_wait_id: String,
    kind: String,
    public_request: Json<JsonObject>,
    resume_metadata: Json<JsonObject>,
    status: String,
    resolution: Option<Json<JsonObject>>,
    requested_at: DateTime<Utc>,
    resolved_at: Option<DateTime<Utc>>,
    cancelled_at: Option<DateTime<Utc>>,
}

impl TryFrom<WaitRow> for RunWait {
    type Error = ExecutionError;

    fn try_from(row: WaitRow) -> Result<Self, Self::Error> {
        Ok(Self {
            wait_id: row.wait_id,
            run_id: row.run_id,
            turn_number: positive_u32("run_waits.turn_number", row.turn_number)?,
            harness_wait_id: row.harness_wait_id,
            kind: row.kind,
            public_request: row.public_request.0,
            resume_metadata: row.resume_metadata.0,
            status: WaitStatus::from_db(&row.status)
                .ok_or_else(|| invalid("run_waits.status", &row.status))?,
            resolution: row.resolution.map(|value| value.0),
            requested_at: row.requested_at,
            resolved_at: row.resolved_at,
            cancelled_at: row.cancelled_at,
        })
    }
}

pub(super) async fn request(
    database: &Database,
    run_id: Uuid,
    command: RequestRunWait,
    token: &LeaseToken,
) -> Result<WaitOutcome<RunWaitRequested>, ExecutionError> {
    command.validate()?;
    let mut transaction = database.pool().begin().await?;

    if let Some(existing) = find_wait(&mut transaction, command.wait_id).await? {
        let wait = RunWait::try_from(existing)?;
        if wait.run_id != run_id
            || wait.harness_wait_id != command.harness_wait_id
            || wait.kind != command.kind
            || wait.public_request != command.public_request
            || wait.resume_metadata != command.resume_metadata
        {
            return Err(ExecutionError::WaitIdConflict(command.wait_id));
        }
        let run = fetch_run(&mut transaction, run_id).await?;
        transaction.commit().await?;
        return Ok(WaitOutcome {
            value: RunWaitRequested { wait, run },
            created: false,
        });
    }

    let run = lock_run(&mut transaction, run_id).await?;
    require_running(&run)?;
    let lease = lock_lease(&mut transaction, run_id).await?;
    verify_lease(&lease, command.lease_version, token)?;
    require_state_version(&run, command.expected_state_version)?;

    let query = format!(
        "insert into run_waits \
         (wait_id, run_id, turn_number, harness_wait_id, kind, public_request, resume_metadata) \
         values ($1, $2, $3, $4, $5, $6, $7) returning {WAIT_COLUMNS}"
    );
    let wait = sqlx::query_as::<_, WaitRow>(&query)
        .bind(command.wait_id)
        .bind(run_id)
        .bind(run.current_turn)
        .bind(&command.harness_wait_id)
        .bind(&command.kind)
        .bind(Json(command.public_request))
        .bind(Json(command.resume_metadata))
        .fetch_one(&mut *transaction)
        .await
        .map_err(|error| match super::constraint(&error) {
            Some("run_waits_pkey") => ExecutionError::WaitIdConflict(command.wait_id),
            Some("run_waits_harness_id_unique") => {
                ExecutionError::HarnessWaitIdConflict(command.harness_wait_id)
            }
            Some("run_waits_one_pending_per_run_idx") => ExecutionError::RunAlreadyWaiting(run_id),
            _ => ExecutionError::Database(error),
        })?;
    let query = format!(
        "update runs set status = 'waiting', state_version = state_version + 1 \
         where run_id = $1 returning {RUN_COLUMNS}"
    );
    let run = sqlx::query_as::<_, RunRow>(&query)
        .bind(run_id)
        .fetch_one(&mut *transaction)
        .await?;
    delete_lease(&mut transaction, run_id).await?;
    let value = RunWaitRequested {
        wait: wait.try_into()?,
        run: run.try_into()?,
    };
    transaction.commit().await?;
    Ok(WaitOutcome {
        value,
        created: true,
    })
}

pub(super) async fn resolve(
    database: &Database,
    wait_id: Uuid,
    command: ResolveRunWait,
) -> Result<WaitOutcome<RunWaitResolved>, ExecutionError> {
    command.validate()?;
    let mut transaction = database.pool().begin().await?;
    let run_id = sqlx::query_scalar::<_, Uuid>("select run_id from run_waits where wait_id = $1")
        .bind(wait_id)
        .fetch_optional(&mut *transaction)
        .await?
        .ok_or(ExecutionError::WaitNotFound(wait_id))?;
    let run = lock_run(&mut transaction, run_id).await?;
    let wait = lock_wait(&mut transaction, wait_id).await?;

    if wait.status == "resolved" {
        if wait.resolution.as_ref().map(|value| &value.0) != Some(&command.resolution) {
            return Err(ExecutionError::WaitNotPending(wait_id));
        }
        let value = RunWaitResolved {
            wait: wait.try_into()?,
            run: run.try_into()?,
        };
        transaction.commit().await?;
        return Ok(WaitOutcome {
            value,
            created: false,
        });
    }
    if wait.status != "pending" {
        return Err(ExecutionError::WaitNotPending(wait_id));
    }
    let status = run_status(&run)?;
    if status != RunStatus::Waiting {
        return Err(ExecutionError::RunNotWaiting { run_id, status });
    }
    require_state_version(&run, command.expected_state_version)?;

    let query = format!(
        "update run_waits set status = 'resolved', resolution = $2, resolved_at = now() \
         where wait_id = $1 returning {WAIT_COLUMNS}"
    );
    let wait = sqlx::query_as::<_, WaitRow>(&query)
        .bind(wait_id)
        .bind(Json(command.resolution))
        .fetch_one(&mut *transaction)
        .await?;
    let query = format!(
        "update runs set status = 'queued', state_version = state_version + 1, queued_at = now() \
         where run_id = $1 returning {RUN_COLUMNS}"
    );
    let run = sqlx::query_as::<_, RunRow>(&query)
        .bind(run_id)
        .fetch_one(&mut *transaction)
        .await?;
    let value = RunWaitResolved {
        wait: wait.try_into()?,
        run: run.try_into()?,
    };
    transaction.commit().await?;
    Ok(WaitOutcome {
        value,
        created: true,
    })
}

pub(super) async fn get(database: &Database, wait_id: Uuid) -> Result<RunWait, ExecutionError> {
    let row = sqlx::query_as::<_, WaitRow>(&format!(
        "select {WAIT_COLUMNS} from run_waits where wait_id = $1"
    ))
    .bind(wait_id)
    .fetch_optional(database.pool())
    .await?
    .ok_or(ExecutionError::WaitNotFound(wait_id))?;
    row.try_into()
}

pub(super) async fn list(
    database: &Database,
    run_id: Option<Uuid>,
    query: WaitListQuery,
) -> Result<RunWaitPage, ExecutionError> {
    let limit = query.limit.unwrap_or(DEFAULT_PAGE_SIZE);
    if !(1..=MAX_PAGE_SIZE).contains(&limit) {
        return Err(ExecutionError::InvalidWaitPageSize);
    }
    let cursor = query.cursor.as_deref().map(decode_cursor).transpose()?;
    let mut sql =
        QueryBuilder::<Postgres>::new(format!("select {WAIT_COLUMNS} from run_waits where true"));
    if let Some(run_id) = run_id {
        sql.push(" and run_id = ").push_bind(run_id);
    }
    if let Some(status) = query.status {
        sql.push(" and status = ").push_bind(status.as_db());
    }
    if let Some(cursor) = cursor {
        sql.push(" and (requested_at, wait_id) > (")
            .push_bind(cursor.requested_at)
            .push(", ")
            .push_bind(cursor.wait_id)
            .push(')');
    }
    sql.push(" order by requested_at, wait_id limit ")
        .push_bind(i64::from(limit) + 1);
    let mut rows = sql
        .build_query_as::<WaitRow>()
        .fetch_all(database.pool())
        .await?;
    let has_more = rows.len() > limit as usize;
    rows.truncate(limit as usize);
    let next_cursor = if has_more {
        rows.last()
            .map(|row| encode_cursor(row.requested_at, row.wait_id))
            .transpose()?
    } else {
        None
    };
    let items = rows
        .into_iter()
        .map(RunWait::try_from)
        .collect::<Result<Vec<_>, _>>()?;
    Ok(RunWaitPage { items, next_cursor })
}

pub(super) async fn resume_for_claim(
    transaction: &mut Transaction<'_, Postgres>,
    run: &RunRow,
) -> Result<Option<WaitResume>, ExecutionError> {
    let row = sqlx::query_as::<_, WaitRow>(&format!(
        "select {WAIT_COLUMNS} from run_waits \
         where run_id = $1 and turn_number = $2 and status = 'resolved' \
         order by resolved_at desc, wait_id desc limit 1"
    ))
    .bind(run.run_id)
    .bind(run.current_turn)
    .fetch_optional(&mut **transaction)
    .await?;
    row.map(|row| {
        let resolution = row
            .resolution
            .ok_or_else(|| invalid("run_waits.resolution", "is unexpectedly null"))?;
        let resolved_at = row
            .resolved_at
            .ok_or_else(|| invalid("run_waits.resolved_at", "is unexpectedly null"))?;
        Ok(WaitResume {
            wait_id: row.wait_id,
            harness_wait_id: row.harness_wait_id,
            kind: row.kind,
            public_request: row.public_request.0,
            resume_metadata: row.resume_metadata.0,
            resolution: resolution.0,
            resolved_at,
        })
    })
    .transpose()
}

pub(super) async fn cancel_pending(
    transaction: &mut Transaction<'_, Postgres>,
    run_id: Uuid,
) -> Result<Option<JsonObject>, ExecutionError> {
    let metadata = sqlx::query_scalar::<_, Json<JsonObject>>(
        "update run_waits set status = 'cancelled', cancelled_at = now() \
         where run_id = $1 and status = 'pending' returning resume_metadata",
    )
    .bind(run_id)
    .fetch_optional(&mut **transaction)
    .await?;
    Ok(metadata.map(|value| value.0))
}

async fn find_wait(
    transaction: &mut Transaction<'_, Postgres>,
    wait_id: Uuid,
) -> Result<Option<WaitRow>, ExecutionError> {
    sqlx::query_as::<_, WaitRow>(&format!(
        "select {WAIT_COLUMNS} from run_waits where wait_id = $1"
    ))
    .bind(wait_id)
    .fetch_optional(&mut **transaction)
    .await
    .map_err(Into::into)
}

async fn lock_wait(
    transaction: &mut Transaction<'_, Postgres>,
    wait_id: Uuid,
) -> Result<WaitRow, ExecutionError> {
    sqlx::query_as::<_, WaitRow>(&format!(
        "select {WAIT_COLUMNS} from run_waits where wait_id = $1 for update"
    ))
    .bind(wait_id)
    .fetch_optional(&mut **transaction)
    .await?
    .ok_or(ExecutionError::WaitNotFound(wait_id))
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

#[derive(Serialize, Deserialize)]
struct WaitCursor {
    requested_at_us: i64,
    wait_id: Uuid,
}

struct DecodedCursor {
    requested_at: DateTime<Utc>,
    wait_id: Uuid,
}

fn encode_cursor(requested_at: DateTime<Utc>, wait_id: Uuid) -> Result<String, ExecutionError> {
    let bytes = serde_json::to_vec(&WaitCursor {
        requested_at_us: requested_at.timestamp_micros(),
        wait_id,
    })
    .map_err(|_| ExecutionError::InvalidWaitCursor)?;
    Ok(URL_SAFE_NO_PAD.encode(bytes))
}

fn decode_cursor(value: &str) -> Result<DecodedCursor, ExecutionError> {
    let bytes = URL_SAFE_NO_PAD
        .decode(value)
        .map_err(|_| ExecutionError::InvalidWaitCursor)?;
    let cursor: WaitCursor =
        serde_json::from_slice(&bytes).map_err(|_| ExecutionError::InvalidWaitCursor)?;
    let requested_at = DateTime::from_timestamp_micros(cursor.requested_at_us)
        .ok_or(ExecutionError::InvalidWaitCursor)?;
    Ok(DecodedCursor {
        requested_at,
        wait_id: cursor.wait_id,
    })
}

fn invalid(field: &str, error: impl std::fmt::Display) -> ExecutionError {
    ExecutionError::InvalidStoredData(format!("{field}: {error}"))
}
