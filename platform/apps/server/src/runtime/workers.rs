use super::{
    RuntimeService,
    error::{Result, RuntimeError},
    model::{SequenceQuery, after, limit, nonempty},
    mutations,
    queries::sequence_page,
    receipts,
    worker_model::*,
};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use sqlx::{Postgres, Transaction};
use uuid::Uuid;

pub(super) const LEASE_SECONDS: i32 = 60;
pub(super) fn token_hash(token: &str) -> Vec<u8> {
    Sha256::digest(token.as_bytes()).to_vec()
}
// Compare fixed-sized hashes without early exits on matching prefixes.
pub(super) fn token_matches(a: &[u8], b: &[u8]) -> bool {
    a.len() == 32 && b.len() == 32 && a.iter().zip(b).fold(0u8, |diff, (a, b)| diff | (a ^ b)) == 0
}
type Tx<'a> = Transaction<'a, Postgres>;
pub(super) async fn run_json(tx: &mut Tx<'_>, id: Uuid) -> Result<Value> {
    Ok(
        sqlx::query_scalar("select to_jsonb(r) from runs r where id=$1")
            .bind(id)
            .fetch_one(&mut **tx)
            .await?,
    )
}
pub(super) async fn worker_json(tx: &mut Tx<'_>, id: Uuid) -> Result<Value> {
    Ok(
        sqlx::query_scalar("select to_jsonb(w) from workers w where id=$1")
            .bind(id)
            .fetch_one(&mut **tx)
            .await?,
    )
}
/// All mutations involving multiple runs take sessions, then runs, in UUID order.
/// Claim/heartbeat touch runs only and never subsequently lock sessions.
pub(super) async fn lock_runs(tx: &mut Tx<'_>, ids: &[Uuid]) -> Result<()> {
    let mut ids = ids.to_vec();
    ids.sort();
    ids.dedup();
    let sessions: Vec<Uuid> = sqlx::query_scalar(
        "select distinct session_id from runs where id=any($1) order by session_id",
    )
    .bind(&ids)
    .fetch_all(&mut **tx)
    .await?;
    for session in sessions {
        mutations::session(tx, session).await?;
    }
    for id in ids {
        mutations::run(tx, id).await?;
    }
    Ok(())
}
pub(super) async fn owned(tx: &mut Tx<'_>, id: Uuid, owner: Owner) -> Result<()> {
    let valid: bool = sqlx::query_scalar("select exists(select 1 from runs r join workers w on w.id=r.worker_id where r.id=$1 and r.status='running' and r.worker_id=$2 and r.lease_epoch=$3 and r.lease_expires_at>clock_timestamp() and w.status<>'offline')")
        .bind(id).bind(owner.worker_id).bind(owner.lease_epoch).fetch_one(&mut **tx).await?;
    if !valid {
        return Err(RuntimeError::CodedConflict(
            platform_runtime_contracts::ConflictCode::LeaseLost,
            "Execution ownership was lost. Reconcile assignments before continuing.",
        ));
    }
    Ok(())
}
async fn assignments(tx: &mut Tx<'_>, worker: Uuid) -> Result<Vec<Value>> {
    Ok(sqlx::query_scalar("select to_jsonb(r) || jsonb_build_object('harness_id',s.harness_id) from runs r join sessions s on s.id=r.session_id where r.worker_id=$1 and r.status='running' and r.lease_expires_at>clock_timestamp() order by r.id")
        .bind(worker).fetch_all(&mut **tx).await?)
}
impl RuntimeService {
    pub(super) async fn authenticate_worker(&self, id: Uuid, token: &str) -> Result<()> {
        let hash: Option<Vec<u8>> =
            sqlx::query_scalar("select token_hash from worker_credentials where worker_id=$1")
                .bind(id)
                .fetch_optional(&self.pool)
                .await?;
        if !hash.is_some_and(|hash| token_matches(&hash, &token_hash(token))) {
            return Err(RuntimeError::Unauthorized);
        }
        Ok(())
    }
    pub(super) async fn register_worker(&self, id: Uuid, request: RegisterWorker) -> Result<Value> {
        nonempty(
            &request.build_id,
            256,
            "Provide a build_id of 1–256 characters.",
        )?;
        if request.capacity < 1 || request.supported_harnesses.len() > 200 {
            return Err(RuntimeError::Invalid(
                "Positive capacity and at most 200 supported harnesses are required.",
            ));
        }
        if !(32..=256).contains(&request.worker_token.len())
            || !request
                .worker_token
                .bytes()
                .all(|b| (33..=126).contains(&b))
        {
            return Err(RuntimeError::Invalid(
                "worker_token must contain 32–256 printable ASCII characters without spaces.",
            ));
        }
        let mut harnesses = request.supported_harnesses;
        harnesses.sort();
        harnesses.dedup();
        for h in &harnesses {
            if !platform_runtime_contracts::is_valid_harness_id(h) {
                return Err(RuntimeError::Invalid("Invalid harness ID."));
            }
        }
        let mut tx = self.transaction().await?;
        // Serialize simultaneous registrations, including the absent-row case.
        sqlx::query("select pg_advisory_xact_lock(hashtextextended($1,1))")
            .bind(id.to_string())
            .execute(&mut *tx)
            .await?;
        let old: Option<Value> =
            sqlx::query_scalar("select to_jsonb(w) from workers w where id=$1 for update")
                .bind(id)
                .fetch_optional(&mut *tx)
                .await?;
        if let Some(old) = old {
            let hash: Option<Vec<u8>> =
                sqlx::query_scalar("select token_hash from worker_credentials where worker_id=$1")
                    .bind(id)
                    .fetch_optional(&mut *tx)
                    .await?;
            if old["build_id"] != request.build_id
                || old["supported_harnesses"] != json!(harnesses)
                || !hash.is_some_and(|h| token_matches(&h, &token_hash(&request.worker_token)))
            {
                return Err(RuntimeError::CodedConflict(
                    platform_runtime_contracts::ConflictCode::WorkerIdentityConflict,
                    "Worker identity is already registered. Use a new UUID for a new process.",
                ));
            }
            // PUT retries never reset drain status, capacity, or leases.
            return Ok(old);
        }
        let count: i64 = sqlx::query_scalar("select count(*) from harnesses where id=any($1)")
            .bind(&harnesses)
            .fetch_one(&mut *tx)
            .await?;
        if count != harnesses.len() as i64 {
            return Err(RuntimeError::Invalid(
                "Every supported harness must be registered.",
            ));
        }
        sqlx::query(
            "insert into workers(id,build_id,supported_harnesses,capacity) values($1,$2,$3,$4)",
        )
        .bind(id)
        .bind(request.build_id)
        .bind(harnesses)
        .bind(request.capacity)
        .execute(&mut *tx)
        .await?;
        sqlx::query("insert into worker_credentials(worker_id,token_hash) values($1,$2)")
            .bind(id)
            .bind(token_hash(&request.worker_token))
            .execute(&mut *tx)
            .await?;
        let result = worker_json(&mut tx, id).await?;
        tx.commit().await?;
        Ok(result)
    }
    pub(super) async fn patch_worker(&self, id: Uuid, request: PatchWorker) -> Result<Value> {
        if request.status.is_none() && request.capacity.is_none()
            || request.capacity.is_some_and(|c| c < 1)
        {
            return Err(RuntimeError::Invalid(
                "Provide a status or positive capacity.",
            ));
        }
        let mut tx = self.transaction().await?;
        let status: String =
            sqlx::query_scalar("select status from workers where id=$1 for update")
                .bind(id)
                .fetch_one(&mut *tx)
                .await?;
        if status == "offline" && request.status.is_some_and(|s| s.as_str() != "offline") {
            return Err(RuntimeError::CodedConflict(
                platform_runtime_contracts::ConflictCode::WorkerStateConflict,
                "Offline worker identities cannot restart. Register a new UUID.",
            ));
        }
        if request.status.is_some_and(|s| s.as_str() == "offline") {
            let active:bool=sqlx::query_scalar("select exists(select 1 from runs where worker_id=$1 and lease_expires_at>clock_timestamp())").bind(id).fetch_one(&mut *tx).await?;
            if active {
                return Err(RuntimeError::CodedConflict(
                    platform_runtime_contracts::ConflictCode::WorkerStateConflict,
                    "Drain and release active assignments before going offline.",
                ));
            }
        }
        sqlx::query("update workers set status=coalesce($2,status),capacity=coalesce($3,capacity),last_seen_at=clock_timestamp() where id=$1").bind(id).bind(request.status.map(WorkerStatus::as_str)).bind(request.capacity).execute(&mut *tx).await?;
        let result = worker_json(&mut tx, id).await?;
        tx.commit().await?;
        Ok(result)
    }
    pub(super) async fn heartbeat(&self, id: Uuid, request: Heartbeat) -> Result<Value> {
        if request.leases.len() > 1000 {
            return Err(RuntimeError::Invalid(
                "Heartbeat at most 1000 leases per request.",
            ));
        }
        let mut tx = self.transaction().await?;
        let status: String =
            sqlx::query_scalar("select status from workers where id=$1 for update")
                .bind(id)
                .fetch_one(&mut *tx)
                .await?;
        if status == "offline" {
            return Err(RuntimeError::CodedConflict(
                platform_runtime_contracts::ConflictCode::WorkerStateConflict,
                "This worker is offline.",
            ));
        }
        sqlx::query("update workers set last_seen_at=clock_timestamp() where id=$1")
            .bind(id)
            .execute(&mut *tx)
            .await?;
        let mut leases = request.leases;
        leases.sort_by_key(|l| l.run_id);
        if leases.windows(2).any(|w| w[0].run_id == w[1].run_id)
            || leases.iter().any(|l| l.lease_epoch < 1)
        {
            return Err(RuntimeError::Invalid(
                "Provide unique runs with positive lease epochs.",
            ));
        }
        let mut renewed = Vec::new();
        let mut lost = Vec::new();
        for lease in leases {
            let row:Option<Value>=sqlx::query_scalar("update runs set lease_expires_at=clock_timestamp()+make_interval(secs=>$4::double precision) where id=$1 and worker_id=$2 and lease_epoch=$3 and status='running' and lease_expires_at>clock_timestamp() returning jsonb_build_object('run_id',id,'lease_epoch',lease_epoch,'lease_expires_at',lease_expires_at,'abort_requested_at',abort_requested_at,'version',version)")
                .bind(lease.run_id).bind(id).bind(lease.lease_epoch).bind(LEASE_SECONDS).fetch_optional(&mut *tx).await?;
            match row {
                Some(row) => renewed.push(row),
                None => lost.push(lease),
            }
        }
        tx.commit().await?;
        Ok(json!({"renewed":renewed,"lost":lost,"lease_duration_seconds":LEASE_SECONDS}))
    }
    pub(super) async fn assignments(&self, id: Uuid) -> Result<Value> {
        let mut tx = self.transaction().await?;
        let result =
            json!({"items":assignments(&mut tx,id).await?,"worker":worker_json(&mut tx,id).await?});
        tx.commit().await?;
        Ok(result)
    }
    pub(super) async fn claim(&self, id: Uuid, key: &str, request: Claim) -> Result<Value> {
        if request.limit < 1 || request.limit > 200 {
            return Err(RuntimeError::Invalid(
                "Claim limit must be between 1 and 200.",
            ));
        }
        let hash = receipts::hash(&request)?;
        let mut tx = self.transaction().await?;
        let (status, capacity, harnesses): (String, i32, Vec<String>) = sqlx::query_as(
            "select status,capacity,supported_harnesses from workers where id=$1 for update",
        )
        .bind(id)
        .fetch_one(&mut *tx)
        .await?;
        let replay: Option<(String, Value)> = sqlx::query_as(
            "select request_hash,result from worker_claim_requests where worker_id=$1 and key=$2",
        )
        .bind(id)
        .bind(key)
        .fetch_optional(&mut *tx)
        .await?;
        if let Some((saved, result)) = replay {
            if saved != hash {
                return Err(RuntimeError::CodedConflict(
                    platform_runtime_contracts::ConflictCode::IdempotencyKeyConflict,
                    "Idempotency key was used with a different claim.",
                ));
            }
            return Ok(result);
        }
        if status != "accepting" {
            return Err(RuntimeError::CodedConflict(
                platform_runtime_contracts::ConflictCode::WorkerStateConflict,
                "This worker is not accepting assignments.",
            ));
        }
        sqlx::query("update workers set last_seen_at=clock_timestamp() where id=$1")
            .bind(id)
            .execute(&mut *tx)
            .await?;
        let active: i64 = sqlx::query_scalar(
            "select count(*) from runs where worker_id=$1 and lease_expires_at>clock_timestamp()",
        )
        .bind(id)
        .fetch_one(&mut *tx)
        .await?;
        let take = (i64::from(capacity) - active)
            .max(0)
            .min(i64::from(request.limit));
        // Expired running work is eligible even if the old worker still heartbeats.
        // Its epoch advances on takeover, so stale work cannot publish results.
        let ids:Vec<(Uuid,i64)>=sqlx::query_as("select r.id,r.lease_epoch from runs r join sessions s on s.id=r.session_id where s.harness_id=any($1) and ((r.status='ready' and r.available_at<=clock_timestamp()) or (r.status='running' and r.lease_expires_at<=clock_timestamp())) order by coalesce(r.available_at,r.lease_expires_at),r.id limit $2 for update of r skip locked")
            .bind(harnesses).bind(take).fetch_all(&mut *tx).await?;
        let mut items = Vec::new();
        for (run, epoch) in &ids {
            sqlx::query("update runs set status='running',available_at=null,worker_id=$2,lease_epoch=lease_epoch+1,lease_expires_at=clock_timestamp()+make_interval(secs=>$3::double precision),started_at=coalesce(started_at,clock_timestamp()),version=version+1 where id=$1")
                .bind(run).bind(id).bind(LEASE_SECONDS).execute(&mut *tx).await?;
            mutations::event(
                &mut tx,
                *run,
                if *epoch == 0 {
                    "run.started"
                } else {
                    "run.resumed"
                },
                json!({"worker_id":id,"lease_epoch":epoch+1}),
            )
            .await?;
            mutations::hint(&mut tx, *run).await?;
            let item:Value=sqlx::query_scalar("select to_jsonb(r)||jsonb_build_object('harness_id',s.harness_id) from runs r join sessions s on s.id=r.session_id where r.id=$1").bind(run).fetch_one(&mut *tx).await?;
            items.push(item);
        }
        let result = json!({"items":items,"lease_duration_seconds":LEASE_SECONDS});
        sqlx::query("insert into worker_claim_requests(worker_id,key,request_hash,result) values($1,$2,$3,$4)").bind(id).bind(key).bind(hash).bind(&result).execute(&mut *tx).await?;
        tx.commit().await?;
        for (run, _) in ids {
            self.notify(run);
        }
        Ok(result)
    }
    pub(super) async fn context(&self, id: Uuid, owner: Owner, q: ContextQuery) -> Result<Value> {
        let count = limit(q.limit)?;
        let revision = after(q.after_revision)?;
        let mut tx = self.transaction().await?;
        lock_runs(&mut tx, &[id]).await?;
        owned(&mut tx, id, owner).await?;
        let run = run_json(&mut tx, id).await?;
        let session: Value = sqlx::query_scalar(
            "select to_jsonb(s) from sessions s join runs r on r.session_id=s.id where r.id=$1",
        )
        .bind(id)
        .fetch_one(&mut *tx)
        .await?;
        let messages:Vec<Value>=sqlx::query_scalar("select jsonb_build_object('message_id',m.id,'revision',sm.revision,'run_id',sm.run_id,'origin_run_id',m.origin_run_id,'message',m.message,'created_at',m.created_at) from session_messages sm join messages m on m.id=sm.message_id join runs r on r.session_id=sm.session_id where r.id=$1 and sm.revision>$2 order by sm.revision limit $3").bind(id).bind(revision).bind(count+1).fetch_all(&mut *tx).await?;
        let checkpoint: Option<Value> =
            sqlx::query_scalar("select to_jsonb(c) from run_checkpoints c where run_id=$1")
                .bind(id)
                .fetch_optional(&mut *tx)
                .await?;
        let waits:Vec<Value>=sqlx::query_scalar("select to_jsonb(w)||jsonb_build_object('dependencies',(select coalesce(jsonb_agg(to_jsonb(d) order by d.id),'[]'::jsonb) from run_wait_dependencies d where d.wait_id=w.id)) from run_waits w where w.run_id=$1 and ($2::uuid is null or w.id>$2) order by w.id limit $3")
            .bind(id).bind(q.after_wait_id).bind(count+1).fetch_all(&mut *tx).await?;
        let result = json!({"run":run,"session":session,"checkpoint":checkpoint,"messages":sequence_page(messages,count,"revision","next_after_revision"),"waits":sequence_page(waits,count,"id","next_after_wait_id")});
        tx.commit().await?;
        Ok(result)
    }
    pub(super) async fn worker_inputs(
        &self,
        id: Uuid,
        owner: Owner,
        q: SequenceQuery,
    ) -> Result<Value> {
        let count = limit(q.limit)?;
        let seq = after(q.after_sequence)?;
        let status = q.status.as_deref().unwrap_or("pending");
        if !["pending", "handled", "rejected", "all"].contains(&status) {
            return Err(RuntimeError::Invalid("Invalid input status."));
        }
        let mut tx = self.transaction().await?;
        lock_runs(&mut tx, &[id]).await?;
        owned(&mut tx, id, owner).await?;
        let items:Vec<Value>=sqlx::query_scalar("select to_jsonb(i)-'deduplication_key' from run_inputs i where run_id=$1 and sequence>$2 and ($3='all' or status=$3) order by sequence limit $4").bind(id).bind(seq).bind(status).bind(count+1).fetch_all(&mut *tx).await?;
        tx.commit().await?;
        Ok(sequence_page(
            items,
            count,
            "sequence",
            "next_after_sequence",
        ))
    }
}
