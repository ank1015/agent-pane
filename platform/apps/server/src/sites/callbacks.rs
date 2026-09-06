//! PostgreSQL outbox -> immutable sites-service invocation receipts.
//! Unknown transport outcomes reuse an invocation ID. Only a known terminal
//! failure permits a fresh execution; logical event IDs never change.
use super::{Result, SitesService};
use serde::Deserialize;
use serde_json::{Value, json};
use sqlx::FromRow;
use std::time::Duration;
use uuid::Uuid;

#[derive(Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct List {
    pub limit: Option<i64>,
    pub after: Option<Uuid>,
}

pub(crate) async fn list(pool: &sqlx::PgPool, site: Uuid, input: List) -> Result<Value> {
    let limit = input.limit.unwrap_or(20);
    if !(1..=50).contains(&limit) {
        return Err(super::invalid("Callback limit must be 1–50."));
    }
    // Payloads can be 16 KiB each. Keep inventory pages small enough for the
    // bounded SDK bridge; individual get() retains full payload/event data.
    let summary =
        format!("({VIEW}) - 'payload' || jsonb_build_object('delivery',to_jsonb(d)-'event')");
    let items: Vec<Value> = sqlx::query_scalar(&format!(
        "select {summary} from site_callbacks c {JOINS} where c.site_id=$1 and ($2::uuid is null or c.id>$2) order by c.id limit $3"
    )).bind(site).bind(input.after).bind(limit+1).fetch_all(pool).await?;
    let has_more = items.len() > limit as usize;
    let items: Vec<_> = items.into_iter().take(limit as usize).collect();
    Ok(
        json!({"next_after":if has_more {items.last().map(|v|v["id"].clone())}else{None},"items":items}),
    )
}
pub(crate) async fn get(pool: &sqlx::PgPool, site: Uuid, callback: Uuid) -> Result<Value> {
    sqlx::query_scalar(&format!(
        "select {VIEW} from site_callbacks c {JOINS} where c.site_id=$1 and c.id=$2"
    ))
    .bind(site)
    .bind(callback)
    .fetch_optional(pool)
    .await?
    .ok_or(super::Error::NotFound)
}
const JOINS: &str = "join project_sites s on s.id=c.site_id join site_project_access a on a.project_id=s.project_id left join site_callback_deliveries d on d.callback_id=c.id";
const VIEW: &str = "to_jsonb(c) || jsonb_build_object('delivery',to_jsonb(d),'status',case when s.deleted_at is not null then 'cancelled' when d.status in ('delivered','failed','cancelled') then d.status when s.desired_status<>'ready' or not a.enabled then 'paused' else coalesce(d.status,'waiting') end)";

#[derive(FromRow)]
struct Claim {
    id: Uuid,
    site_id: Uuid,
    callback_id: Uuid,
    release_id: Uuid,
    path: String,
    event: Value,
    invocation_id: Option<Uuid>,
    attempts: i32,
}

impl SitesService {
    /// One durable claim/delivery. Safe to call concurrently and across replicas.
    /// A lease fences acknowledgements, not application effects; those use the
    /// event ID and Platform's existing durable idempotency receipts.
    pub async fn deliver_callback_once(&self) -> Result<bool> {
        let mut tx = self.pool.begin().await?;
        // Logical deletion cancels pending work. Suspension/revocation pauses it
        // without consuming retry attempts. No database lock spans the HTTP call.
        sqlx::query("with gone as (select d.id from site_callback_deliveries d join site_callbacks c on c.id=d.callback_id join project_sites s on s.id=c.site_id where s.deleted_at is not null and d.status in ('pending','delivering') limit 100 for update of d skip locked) update site_callback_deliveries d set status='cancelled',lease_id=null,lease_expires_at=null,finished_at=clock_timestamp(),version=version+1 from gone where gone.id=d.id")
            .execute(&mut *tx).await?;
        let claim: Option<Claim> = sqlx::query_as(
            "select d.id,c.site_id,d.callback_id,c.release_id,c.path,d.event,d.invocation_id,d.attempts from site_callback_deliveries d join site_callbacks c on c.id=d.callback_id join project_sites s on s.id=c.site_id join site_project_access a on a.project_id=s.project_id where s.deleted_at is null and s.desired_status='ready' and a.enabled and ((d.status='pending' and d.next_attempt_at<=clock_timestamp()) or (d.status='delivering' and d.lease_expires_at<=clock_timestamp())) order by d.next_attempt_at,d.id limit 1 for update of d skip locked"
        ).fetch_optional(&mut *tx).await?;
        let Some(claim) = claim else {
            tx.commit().await?;
            return Ok(false);
        };
        let lease = Uuid::now_v7();
        let invocation = claim.invocation_id.unwrap_or_else(Uuid::now_v7);
        let mut event = claim.event;
        event["id"] = json!(claim.id);
        let body = json!({"id":invocation,"release_id":claim.release_id,"timeout_ms":30000,
            "callback":{"event_id":claim.id,"subscription_id":claim.callback_id},
            "request":{"method":"POST","path":claim.path,"query":{},"body":event}});
        sqlx::query("insert into site_invocations(site_id,id,request) values($1,$2,$3) on conflict do nothing")
            .bind(claim.site_id).bind(invocation).bind(&body).execute(&mut *tx).await?;
        sqlx::query("update site_callback_deliveries set status='delivering',attempts=attempts+1,version=version+1,lease_id=$2,lease_expires_at=clock_timestamp()+interval '60 seconds',invocation_id=$3,last_invocation_id=$3 where id=$1")
            .bind(claim.id).bind(lease).bind(invocation).execute(&mut *tx).await?;
        tx.commit().await?;

        let result = self
            .client
            .call(
                reqwest::Method::POST,
                &format!("/internal/sites/{}/invocations", claim.site_id),
                Some(&body),
            )
            .await;
        let (success, fresh_execution, error) = match result {
            Ok(v)
                if v["id"] == invocation.to_string()
                    && v["release_id"] == claim.release_id.to_string() =>
            {
                match v["status"].as_str() {
                    Some("succeeded")
                        if v["response"]["status"]
                            .as_u64()
                            .is_some_and(|s| (200..300).contains(&s)) =>
                    {
                        (true, false, None)
                    }
                    Some("succeeded") => (false, true, Some("CALLBACK_HTTP_ERROR")),
                    Some("failed" | "timed_out" | "interrupted") => {
                        (false, true, Some("CALLBACK_EXECUTION_FAILED"))
                    }
                    _ => (false, false, Some("CALLBACK_OUTCOME_UNCERTAIN")),
                }
            }
            _ => (false, false, Some("CALLBACK_SERVICE_UNAVAILABLE")),
        };
        let attempts = claim.attempts + 1;
        let status = if success {
            "delivered"
        } else if attempts >= 12 {
            "failed"
        } else {
            "pending"
        };
        // Capped exponential backoff with small jitter. Time and ownership live
        // in PostgreSQL, so a process restart neither loses nor resets retries.
        let delay = (1i32 << attempts.min(9)) + (Uuid::now_v7().as_u128() % 4) as i32;
        sqlx::query("update site_callback_deliveries set status=$3,last_error=$4,invocation_id=case when $5 then null else invocation_id end,next_attempt_at=clock_timestamp()+make_interval(secs=>$6),lease_id=null,lease_expires_at=null,finished_at=case when $3 in ('delivered','failed') then clock_timestamp() else null end,version=version+1 where id=$1 and lease_id=$2 and status='delivering' and lease_expires_at>clock_timestamp()")
            .bind(claim.id).bind(lease).bind(status).bind(error).bind(fresh_execution).bind(f64::from(delay)).execute(&self.pool).await?;
        Ok(true)
    }

    pub fn spawn_callback_dispatcher(&self) -> tokio::task::JoinHandle<()> {
        let service = self.clone();
        tokio::spawn(async move {
            let mut workers = tokio::task::JoinSet::new();
            for _ in 0..4 {
                let service = service.clone();
                workers.spawn(async move {
                    loop {
                        match service.deliver_callback_once().await {
                            Ok(true) => continue,
                            Ok(false) => {}
                            Err(_) => tracing::warn!(
                                "Site callback dispatch failed; durable polling will retry"
                            ),
                        }
                        tokio::time::sleep(Duration::from_secs(1)).await;
                    }
                });
            }
            // Dropping this JoinSet on shutdown aborts its workers. Leases expire
            // and are recovered by any replica; invocation IDs remain stable.
            let _ = workers.join_next().await;
        })
    }
}
