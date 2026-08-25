use llm_contracts::JsonObject;
use serde_json::json;

use super::{
    ExecutionError, ExecutionPolicy, RunStatus, aborting,
    finishing::fail_locked,
    leasing::{lock_lease, lock_run, require_running},
};
use crate::db::Database;

pub async fn reap_expired_once(
    database: &Database,
    policy: ExecutionPolicy,
) -> Result<usize, ExecutionError> {
    let mut transaction = database.pool().begin().await?;
    let run_ids = sqlx::query_scalar(
        "select r.run_id from runs r \
         join run_leases l on l.run_id = r.run_id \
         where r.status in ('running', 'aborting') and l.expires_at <= now() \
         order by l.expires_at, l.lease_id limit $1 \
         for update of r skip locked",
    )
    .bind(policy.reaper_batch_size())
    .fetch_all(&mut *transaction)
    .await?;

    let mut reaped = 0;
    for run_id in run_ids {
        let run = lock_run(&mut transaction, run_id).await?;
        let lease = lock_lease(&mut transaction, run_id).await?;
        if lease.active {
            continue;
        }
        let status = RunStatus::from_db(&run.status).ok_or_else(|| {
            ExecutionError::InvalidStoredData(format!("unknown run status {:?}", run.status))
        })?;
        if status == RunStatus::Aborting {
            aborting::finalize_expired(&mut transaction, run, &lease).await?;
        } else {
            require_running(&run)?;
            fail_locked(&mut transaction, run, lease_expired_failure()).await?;
        }
        reaped += 1;
    }
    transaction.commit().await?;
    Ok(reaped)
}

#[must_use]
pub fn spawn_reaper(database: Database, policy: ExecutionPolicy) -> tokio::task::JoinHandle<()> {
    tokio::spawn(async move {
        let mut interval = tokio::time::interval(policy.reaper_interval());
        interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
        loop {
            interval.tick().await;
            match reap_expired_once(&database, policy).await {
                Ok(count) if count > 0 => tracing::info!(count, "reaped expired run leases"),
                Ok(_) => {}
                Err(error) => tracing::error!(%error, "run lease reaper failed"),
            }
        }
    })
}

fn lease_expired_failure() -> JsonObject {
    json!({
        "code": "lease_expired",
        "message": "worker lease expired"
    })
    .as_object()
    .expect("lease expiry failure is an object")
    .clone()
}
