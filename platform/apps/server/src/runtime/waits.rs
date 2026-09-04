//! Durable conditions, not a universal agent loop. Polling the database is the
//! correctness path; notifications only reduce latency. Safe on every replica.
use super::{
    RuntimeService,
    error::{Result, RuntimeError},
    model::nonempty,
    mutations,
    worker_model::{Dependency, Wait, WaitMode},
};
use serde_json::json;
use sqlx::{Postgres, Transaction};
use uuid::Uuid;
type Tx<'a> = Transaction<'a, Postgres>;

pub(super) async fn insert(tx: &mut Tx<'_>, project: Uuid, run: Uuid, wait: &Wait) -> Result<Uuid> {
    nonempty(&wait.wait_key, 256, "Provide a valid wait_key.")?;
    if wait.dependencies.is_empty() || wait.dependencies.len() > 200 {
        return Err(RuntimeError::Invalid("A wait requires 1–200 dependencies."));
    }
    let id = Uuid::now_v7();
    sqlx::query("insert into run_waits(id,project_id,run_id,wait_key,mode,deadline_at,metadata) values($1,$2,$3,$4,$5,$6,$7)")
        .bind(id).bind(project).bind(run).bind(&wait.wait_key).bind(match wait.mode{WaitMode::Any=>"any",WaitMode::All=>"all"}).bind(wait.deadline_at).bind(json!(wait.metadata)).execute(&mut **tx).await?;
    for dep in &wait.dependencies {
        let (kind, target, input, correlation, wake) = match dep {
            Dependency::RunCompletion { target_run_id } => {
                ("run_completion", Some(*target_run_id), None, None, None)
            }
            Dependency::Input {
                input_kind,
                correlation_key,
            } => {
                nonempty(input_kind, 128, "Provide a valid input kind.")?;
                (
                    "input",
                    None,
                    Some(input_kind.as_str()),
                    correlation_key.as_deref(),
                    None,
                )
            }
            Dependency::Timer { wake_at } => ("timer", None, None, None, Some(*wake_at)),
            Dependency::Operation { correlation_key } => (
                "operation",
                None,
                None,
                Some(correlation_key.as_str()),
                None,
            ),
        };
        if let Some(key) = correlation {
            nonempty(key, 256, "Provide a valid correlation_key.")?;
        }
        sqlx::query("insert into run_wait_dependencies(id,project_id,wait_id,kind,target_run_id,input_kind,correlation_key,wake_at) values($1,$2,$3,$4,$5,$6,$7,$8)")
            .bind(Uuid::now_v7()).bind(project).bind(id).bind(kind).bind(target).bind(input).bind(correlation).bind(wake).execute(&mut **tx).await?;
    }
    Ok(id)
}
/// Caller owns the run lock. Returns whether at least one wait resolved now.
pub(super) async fn resolve(tx: &mut Tx<'_>, id: Uuid) -> Result<bool> {
    sqlx::query("update run_wait_dependencies d set satisfied_at=clock_timestamp(),result=jsonb_build_object('run_id',r.id,'status',r.status,'final_message_id',r.final_message_id,'error',r.error) from run_waits w,runs r where d.wait_id=w.id and w.run_id=$1 and w.status='pending' and d.satisfied_at is null and d.kind='run_completion' and r.id=d.target_run_id and r.status in ('completed','failed','aborted')")
        .bind(id).execute(&mut **tx).await?;
    sqlx::query("update run_wait_dependencies d set satisfied_at=clock_timestamp(),result=jsonb_build_object('wake_at',d.wake_at) from run_waits w where d.wait_id=w.id and w.run_id=$1 and w.status='pending' and d.satisfied_at is null and d.kind='timer' and d.wake_at<=clock_timestamp()")
        .bind(id).execute(&mut **tx).await?;
    // Matching already-persisted input closes the delivery-before-registration
    // race. Correlation keys should identify a unique logical operation.
    sqlx::query("update run_wait_dependencies d set satisfied_at=clock_timestamp(),result=(select jsonb_build_object('input_id',i.id,'payload',i.payload) from run_inputs i where i.run_id=w.run_id and (d.correlation_key is not null or i.status='pending') and ((d.kind='input' and i.kind=d.input_kind) or (d.kind='operation' and i.kind='operation_result')) and (d.correlation_key is null or i.payload->>'correlation_key'=d.correlation_key) order by i.sequence limit 1) from run_waits w where d.wait_id=w.id and w.run_id=$1 and w.status='pending' and d.satisfied_at is null and d.kind in ('input','operation') and exists(select 1 from run_inputs i where i.run_id=w.run_id and (d.correlation_key is not null or i.status='pending') and ((d.kind='input' and i.kind=d.input_kind) or (d.kind='operation' and i.kind='operation_result')) and (d.correlation_key is null or i.payload->>'correlation_key'=d.correlation_key))")
        .bind(id).execute(&mut **tx).await?;
    let satisfied=sqlx::query("update run_waits w set status='satisfied',resolved_at=clock_timestamp(),result=jsonb_build_object('dependencies',(select jsonb_agg(jsonb_build_object('id',d.id,'kind',d.kind,'result',d.result) order by d.id) from run_wait_dependencies d where d.wait_id=w.id and d.satisfied_at is not null)) where w.run_id=$1 and w.status='pending' and exists(select 1 from run_wait_dependencies d where d.wait_id=w.id and d.satisfied_at is not null) and (w.mode='any' or not exists(select 1 from run_wait_dependencies d where d.wait_id=w.id and d.satisfied_at is null))")
        .bind(id).execute(&mut **tx).await?.rows_affected();
    let timed_out=sqlx::query("update run_waits set status='timed_out',resolved_at=clock_timestamp(),result='{}' where run_id=$1 and status='pending' and deadline_at<=clock_timestamp()")
        .bind(id).execute(&mut **tx).await?.rows_affected();
    let resolved = satisfied + timed_out > 0;
    if resolved {
        // An active worker must observe this resolution before it can park.
        // Otherwise a stale commit can strand the run behind a different wait.
        // wake() versions parked/delayed runs; preserve active ownership here.
        sqlx::query("update runs set version=version+1 where id=$1 and status='running'")
            .bind(id)
            .execute(&mut **tx)
            .await?;
        mutations::event(
            tx,
            id,
            "run.wait_resolved",
            json!({"satisfied":satisfied,"timed_out":timed_out}),
        )
        .await?;
        mutations::wake(tx, id, "wait_resolved").await?;
    }
    Ok(resolved)
}
impl RuntimeService {
    /// Run one bounded reconciliation pass. Useful for deterministic integration
    /// tests and for deployments with an external scheduling loop.
    pub async fn reconcile_once(&self) -> std::result::Result<(), super::error::RuntimeError> {
        // Retention still progresses if runtime reconciliation encounters an error.
        let runtime = self.reconcile_runtime().await;
        let workers = self.reconcile_workers().await;
        let receipts = self.cleanup_receipts().await;
        runtime.and(workers).and(receipts)
    }
    async fn reconcile_runtime(&self) -> Result<()> {
        let expired:Vec<Uuid>=sqlx::query_scalar("select id from runs where status='running' and lease_expires_at<=clock_timestamp() order by lease_expires_at,id limit 100").fetch_all(&self.pool).await?;
        for id in expired {
            let mut tx = self.transaction().await?;
            // No session lock is needed: no transcript/activity mutation follows.
            let locked: Option<Uuid> =
                sqlx::query_scalar("select id from runs where id=$1 for update skip locked")
                    .bind(id)
                    .fetch_optional(&mut *tx)
                    .await?;
            if locked.is_none() {
                continue;
            }
            let changed=sqlx::query("update runs set status='ready',available_at=clock_timestamp(),worker_id=null,lease_expires_at=null,version=version+1 where id=$1 and status='running' and lease_expires_at<=clock_timestamp()")
                .bind(id).execute(&mut *tx).await?.rows_affected();
            if changed > 0 {
                mutations::event(&mut tx, id, "run.lease_expired", json!({})).await?;
                mutations::hint(&mut tx, id).await?;
            }
            tx.commit().await?;
            if changed > 0 {
                self.notify(id);
            }
        }
        let ids:Vec<Uuid>=sqlx::query_scalar("select r.id from runs r where r.status in ('ready','running','waiting') and exists(select 1 from run_waits w where w.run_id=r.id and w.status='pending' and (w.deadline_at<=clock_timestamp() or exists(select 1 from run_wait_dependencies d where d.wait_id=w.id and d.satisfied_at is null and ((d.kind='timer' and d.wake_at<=clock_timestamp()) or (d.kind='run_completion' and exists(select 1 from runs target where target.id=d.target_run_id and target.status in ('completed','failed','aborted'))) or (d.kind in ('input','operation') and exists(select 1 from run_inputs i where i.run_id=r.id and (d.correlation_key is not null or i.status='pending') and ((d.kind='input' and i.kind=d.input_kind) or (d.kind='operation' and i.kind='operation_result')) and (d.correlation_key is null or i.payload->>'correlation_key'=d.correlation_key))))))) order by r.id limit 100")
            .fetch_all(&self.pool).await?;
        for id in ids {
            let mut tx = self.transaction().await?;
            let session: Option<Uuid> = sqlx::query_scalar(
                "select s.id from sessions s join runs r on r.session_id=s.id where r.id=$1 for update of s skip locked",
            )
            .bind(id)
            .fetch_optional(&mut *tx)
            .await?;
            if session.is_none() {
                continue;
            }
            let live: Option<bool> = sqlx::query_scalar(
                "select status in ('ready','running','waiting') from runs where id=$1 for update skip locked",
            ).bind(id).fetch_optional(&mut *tx).await?;
            let changed = if live == Some(true) {
                resolve(&mut tx, id).await?
            } else {
                false
            };
            tx.commit().await?;
            if changed {
                self.notify(id);
            }
        }
        Ok(())
    }
    async fn reconcile_workers(&self) -> Result<()> {
        // Metadata liveness is independent of per-run lease ownership.
        let mut tx = self.transaction().await?;
        sqlx::query("update workers set status='offline' where id in (select w.id from workers w where status<>'offline' and last_seen_at<clock_timestamp()-interval '120 seconds' and not exists(select 1 from runs r where r.worker_id=w.id and r.lease_expires_at>clock_timestamp()) order by last_seen_at,id limit 100 for update of w skip locked)")
            .execute(&mut *tx).await?;
        tx.commit().await?;
        Ok(())
    }
    /// Start once per Platform process. PostgreSQL locks make replicas safe.
    /// Abort this handle when shutting down an embedded server.
    pub fn spawn_reconciler(&self) -> tokio::task::JoinHandle<()> {
        let service = self.clone();
        tokio::spawn(async move {
            let mut interval = tokio::time::interval(std::time::Duration::from_secs(1));
            interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
            loop {
                interval.tick().await;
                if let Err(error) = service.reconcile_once().await {
                    tracing::warn!(%error,"runtime reconciliation pass failed; will retry");
                }
            }
        })
    }
}
