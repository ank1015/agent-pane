use std::time::Duration;

use agent_contracts::{RunEventData, WaitResolutionSource, WaitResume, WaitStatus};
use chrono::Utc;
use llm_contracts::Validate as _;
use sqlx::types::Json;
use tokio::task::JoinHandle;
use tokio_util::sync::CancellationToken;
use uuid::Uuid;

use super::{
    ExecutionError, ResolveRunWait, RunRow, RunStatus, RunWait, RunWaitPage, RunWaitResolved,
    WaitListQuery, WaitRow, events,
    records::{RUN_COLUMNS, positive_u32, positive_u64},
    runs,
};
use crate::db::Database;

const WAIT_COLUMNS: &str = "wait_id, run_id, turn_number, harness_wait_id, kind, public_request, resume_metadata, expires_at, status, resolution_source, resolution, requested_at, resolved_at, cancelled_at";

pub(super) async fn resolve(
    database: &Database,
    run_id: Uuid,
    wait_id: Uuid,
    request: ResolveRunWait,
) -> Result<RunWaitResolved, ExecutionError> {
    request.validate()?;
    resolve_inner(
        database,
        run_id,
        wait_id,
        WaitResolutionSource::External,
        request.resolution,
        Some(request.expected_state_version),
    )
    .await
}

async fn resolve_inner(
    database: &Database,
    run_id: Uuid,
    wait_id: Uuid,
    source: WaitResolutionSource,
    resolution: llm_contracts::JsonObject,
    expected_state_version: Option<u64>,
) -> Result<RunWaitResolved, ExecutionError> {
    let mut tx = database.pool().begin().await?;
    let row = sqlx::query_as::<_, WaitRow>(&format!(
        "select {WAIT_COLUMNS} from run_waits where wait_id = $1 and run_id = $2 for update"
    ))
    .bind(wait_id)
    .bind(run_id)
    .fetch_optional(&mut *tx)
    .await?
    .ok_or(ExecutionError::WaitNotFound(wait_id))?;
    let wait = RunWait::try_from(row)?;
    if wait.status != WaitStatus::Pending {
        return Err(ExecutionError::WaitNotPending(wait_id));
    }
    if source == WaitResolutionSource::Expired
        && wait.expires_at.is_none_or(|expires| expires > Utc::now())
    {
        return Err(ExecutionError::WaitNotPending(wait_id));
    }
    let context = runs::load_context(&mut tx, run_id, true).await?;
    let status = RunStatus::from_db(&context.status).ok_or_else(|| {
        ExecutionError::InvalidStoredData(format!("runs.status: {}", context.status))
    })?;
    if status != RunStatus::Waiting {
        return Err(ExecutionError::RunNotActive { run_id, status });
    }
    let actual = u64::try_from(context.state_version)
        .map_err(|e| ExecutionError::InvalidStoredData(format!("runs.state_version: {e}")))?;
    if let Some(expected) = expected_state_version
        && expected != actual
    {
        return Err(ExecutionError::RunStateConflict { expected, actual });
    }
    let resolved_at = Utc::now();
    sqlx::query("update run_waits set status = 'resolved', resolution_source = $2, resolution = $3, resolved_at = $4 where wait_id = $1")
        .bind(wait_id).bind(source.as_db()).bind(Json(&resolution)).bind(resolved_at).execute(&mut *tx).await?;
    sqlx::query(
        "update runs set status = 'active', state_version = state_version + 1 where run_id = $1",
    )
    .bind(run_id)
    .execute(&mut *tx)
    .await?;
    let resume = WaitResume {
        wait_id,
        harness_wait_id: wait.harness_wait_id,
        kind: wait.kind,
        public_request: wait.public_request,
        resume_metadata: wait.resume_metadata,
        source,
        resolution: resolution.clone(),
        resolved_at,
    };
    let next = runs::load_context(&mut tx, run_id, false).await?;
    events::append_agent(
        &mut tx,
        run_id,
        Some(positive_u32("runs.current_turn", next.current_turn)?),
        positive_u64("runs.state_version", next.state_version)?,
        RunStatus::Active,
        Uuid::now_v7(),
        resolved_at,
        RunEventData::RunResumed { wait_id, source },
    )
    .await?;
    runs::enqueue_turn(&mut tx, &next, Some(resume)).await?;
    let wait_row = sqlx::query_as::<_, WaitRow>(&format!(
        "select {WAIT_COLUMNS} from run_waits where wait_id = $1"
    ))
    .bind(wait_id)
    .fetch_one(&mut *tx)
    .await?;
    let run_row =
        sqlx::query_as::<_, RunRow>(&format!("select {RUN_COLUMNS} from runs where run_id = $1"))
            .bind(run_id)
            .fetch_one(&mut *tx)
            .await?;
    let outcome = RunWaitResolved {
        wait: wait_row.try_into()?,
        run: run_row.try_into()?,
    };
    tx.commit().await?;
    Ok(outcome)
}

pub(super) async fn get(
    database: &Database,
    run_id: Uuid,
    wait_id: Uuid,
) -> Result<RunWait, ExecutionError> {
    let row = sqlx::query_as::<_, WaitRow>(&format!(
        "select {WAIT_COLUMNS} from run_waits where wait_id = $1 and run_id = $2"
    ))
    .bind(wait_id)
    .bind(run_id)
    .fetch_optional(database.pool())
    .await?
    .ok_or(ExecutionError::WaitNotFound(wait_id))?;
    row.try_into()
}

pub(super) async fn list(
    database: &Database,
    run_id: Uuid,
    query: WaitListQuery,
) -> Result<RunWaitPage, ExecutionError> {
    runs::get(database, run_id).await?;
    if query.cursor.is_some() {
        return Err(ExecutionError::InvalidWaitCursor);
    }
    let limit = query.limit.unwrap_or(50);
    if !(1..=100).contains(&limit) {
        return Err(ExecutionError::InvalidWaitPageSize);
    }
    let mut sql = format!("select {WAIT_COLUMNS} from run_waits where run_id = $1");
    if query.status.is_some() {
        sql.push_str(" and status = $3");
    }
    sql.push_str(" order by requested_at, wait_id limit $2");
    let mut statement = sqlx::query_as::<_, WaitRow>(&sql)
        .bind(run_id)
        .bind(i64::from(limit));
    if let Some(status) = query.status {
        statement = statement.bind(status.as_db());
    }
    let items = statement
        .fetch_all(database.pool())
        .await?
        .into_iter()
        .map(RunWait::try_from)
        .collect::<Result<Vec<_>, _>>()?;
    Ok(RunWaitPage {
        items,
        next_cursor: None,
    })
}

pub async fn expire_waits_once(
    database: &Database,
    batch_size: i64,
) -> Result<usize, ExecutionError> {
    let ids = sqlx::query_as::<_, (Uuid, Uuid)>(
        "select wait_id, run_id from run_waits where status = 'pending' and expires_at <= now() order by expires_at, wait_id limit $1",
    ).bind(batch_size).fetch_all(database.pool()).await?;
    let mut resolved = 0;
    for (wait_id, run_id) in ids {
        match resolve_inner(
            database,
            run_id,
            wait_id,
            WaitResolutionSource::Expired,
            llm_contracts::JsonObject::new(),
            None,
        )
        .await
        {
            Ok(_) => resolved += 1,
            Err(ExecutionError::WaitNotPending(_) | ExecutionError::RunNotActive { .. }) => {}
            Err(error) => return Err(error),
        }
    }
    Ok(resolved)
}

pub fn spawn_wait_expiry(
    database: Database,
    interval: Duration,
    batch_size: i64,
    shutdown: CancellationToken,
) -> JoinHandle<()> {
    tokio::spawn(async move {
        let mut ticker = tokio::time::interval(interval);
        ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
        loop {
            tokio::select! {
                () = shutdown.cancelled() => break,
                _ = ticker.tick() => match expire_waits_once(&database, batch_size).await {
                    Ok(count) if count > 0 => tracing::info!(count, "resolved expired run waits"),
                    Ok(_) => {},
                    Err(error) => tracing::warn!(%error, "could not resolve expired run waits"),
                }
            }
        }
    })
}
