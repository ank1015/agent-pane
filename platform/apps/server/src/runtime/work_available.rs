//! Advisory long polls. Durable claims/leases remain the ownership authority.
use super::{
    RuntimeService,
    error::{Result, RuntimeError},
};
use platform_runtime_contracts::{
    ConflictCode, MAX_WORK_WAIT_SECONDS, WorkAvailability, WorkAvailabilityQuery,
};
use std::time::Duration;
use tokio::time::Instant;
use uuid::Uuid;

impl RuntimeService {
    pub(super) async fn work_available(
        &self,
        worker: Uuid,
        query: WorkAvailabilityQuery,
    ) -> Result<WorkAvailability> {
        let seconds = query.wait_seconds.unwrap_or(MAX_WORK_WAIT_SECONDS);
        if seconds > MAX_WORK_WAIT_SECONDS {
            return Err(RuntimeError::Invalid(
                "wait_seconds must be between 0 and 25.",
            ));
        }
        // Subscribe BEFORE reading: a commit between the read and the wait must
        // remain observable. Cross-replica hints arrive via the existing PG listener.
        let mut hints = self.signals.subscribe();
        let deadline = Instant::now() + Duration::from_secs(u64::from(seconds));
        loop {
            // A short, read-only statement, never a connection or lock held across
            // the wait. Use the same eligibility/capacity predicates as claims.
            let (status, available): (String, bool) = sqlx::query_as(
                "select w.status, case when w.status='accepting'
                   and (select count(*) from runs where worker_id=w.id
                        and lease_expires_at>clock_timestamp()) < w.capacity
                 then exists(
                   select 1 from runs r join sessions s on s.id=r.session_id
                   where s.harness_id=any(w.supported_harnesses)
                   and ((r.status='ready' and r.available_at<=clock_timestamp())
                     or (r.status='running' and r.lease_expires_at<=clock_timestamp()))
                 ) else false end
                 from workers w where w.id=$1",
            )
            .bind(worker)
            .fetch_optional(&self.pool)
            .await?
            .ok_or(RuntimeError::NotFound)?;
            if status == "offline" {
                // This endpoint's WorkerStateConflict specifically means offline.
                return Err(RuntimeError::CodedConflict(
                    ConflictCode::WorkerStateConflict,
                    "This worker is offline. Register a new process identity.",
                ));
            }
            if available || Instant::now() >= deadline {
                return Ok(WorkAvailability { available });
            }
            // Also catches delayed availability, lease expiry, dropped hints, and
            // PG LISTEN reconnect gaps without requiring another notification.
            let fallback = (Instant::now() + Duration::from_secs(5)).min(deadline);
            tokio::select! {
                _ = tokio::time::sleep_until(fallback) => {}
                _ = hints.recv() => {
                    // Coalesce bursts and bound reads under unrelated run traffic.
                    tokio::time::sleep_until((Instant::now() + Duration::from_millis(50)).min(deadline)).await;
                    while hints.try_recv().is_ok() {}
                }
            }
        }
    }
}
