use super::{
    RuntimeService,
    error::{Result, RuntimeError},
    model::nonempty,
    mutations,
    receipts::{self, Reply},
    worker_model::*,
    workers::{lock_runs, owned, run_json},
};
use llm_contracts::Validate;
use serde_json::{Value, json};
use sqlx::{Postgres, Transaction};
use uuid::Uuid;

pub(super) fn validate_events(events: &[HarnessEvent]) -> Result<()> {
    if events.len() > 200 {
        return Err(RuntimeError::Invalid(
            "Append at most 200 events per request.",
        ));
    }
    for e in events {
        nonempty(&e.r#type, 128, "Provide a valid event type.")?;
        if e.r#type.contains(['\r', '\n']) {
            return Err(RuntimeError::Invalid(
                "Event types cannot contain line breaks.",
            ));
        }
        if e.r#type.starts_with("run.") {
            return Err(RuntimeError::Invalid(
                "The run.* event namespace is reserved for Platform.",
            ));
        }
    }
    Ok(())
}
async fn append_events(
    tx: &mut Transaction<'_, Postgres>,
    id: Uuid,
    events: &[HarnessEvent],
) -> Result<Vec<Value>> {
    let mut result = Vec::new();
    for e in events {
        let event:Value=sqlx::query_scalar("insert into run_events(id,run_id,type,source,payload,occurred_at) values($1,$2,$3,'harness',$4,coalesce($5,clock_timestamp())) returning to_jsonb(run_events)")
            .bind(Uuid::now_v7()).bind(id).bind(&e.r#type).bind(json!(e.payload)).bind(e.occurred_at).fetch_one(&mut **tx).await?;
        result.push(event);
    }
    Ok(result)
}
impl RuntimeService {
    pub(super) async fn worker_events(
        &self,
        id: Uuid,
        owner: Owner,
        key: &str,
        request: Events,
    ) -> Result<Reply> {
        validate_events(&request.events)?;
        if request.events.is_empty() {
            return Err(RuntimeError::Invalid("Provide at least one event."));
        }
        let scope = "run.worker_events";
        let hash = receipts::hash(&request)?;
        let mut tx = self.transaction().await?;
        if let Some(reply) = receipts::worker_replay(&mut tx, scope, id, key, &hash, owner).await? {
            return Ok(reply);
        }
        lock_runs(&mut tx, &[id]).await?;
        owned(&mut tx, id, owner).await?;
        let r = mutations::run(&mut tx, id).await?;
        let reply = Reply::new(
            201,
            json!({"items":append_events(&mut tx,id,&request.events).await?}),
        )
        .owned_by(owner);
        owned(&mut tx, id, owner).await?;
        mutations::hint(&mut tx, id).await?;
        receipts::save(&mut tx, r.project_id, scope, id, key, &hash, &reply).await?;
        tx.commit().await?;
        self.notify(id);
        Ok(reply)
    }
    pub(super) async fn commit(
        &self,
        id: Uuid,
        owner: Owner,
        key: &str,
        request: Commit,
    ) -> Result<Reply> {
        validate_events(&request.events)?;
        if request.expected_run_version < 1
            || request.expected_session_revision < 0
            || request
                .checkpoint
                .as_ref()
                .is_some_and(|c| c.expected_version < 0)
        {
            return Err(RuntimeError::Invalid("Provide valid expected versions."));
        }
        super::session_state::validate(&request.session_state)?;
        if request.messages.len() > 200
            || request.input_results.len() > 200
            || request.waits.len() > 200
            || request.cancel_wait_ids.len() > 200
        {
            return Err(RuntimeError::Invalid(
                "Commit batches are limited to 200 items per collection.",
            ));
        }
        for m in &request.messages {
            m.message
                .validate()
                .map_err(|_| RuntimeError::Invalid("Invalid canonical message."))?;
        }
        let scope = "run.commit";
        let hash = receipts::hash(&request)?;
        let mut tx = self.transaction().await?;
        // Replay before checking the lease: a successful terminal/release commit
        // must still be retrievable after ownership has ended.
        if let Some(reply) = receipts::worker_replay(&mut tx, scope, id, key, &hash, owner).await? {
            return Ok(reply);
        }
        lock_runs(&mut tx, &[id]).await?;
        owned(&mut tx, id, owner).await?;
        let r = mutations::run(&mut tx, id).await?;
        let s = mutations::session(&mut tx, r.session_id).await?;
        let version: i64 = sqlx::query_scalar("select version from runs where id=$1")
            .bind(id)
            .fetch_one(&mut *tx)
            .await?;
        if version != request.expected_run_version {
            return Err(RuntimeError::CodedConflict(
                platform_runtime_contracts::ConflictCode::RunVersionConflict,
                "Run version changed. Reload context before committing.",
            ));
        }
        if s.current_revision != request.expected_session_revision {
            return Err(RuntimeError::CodedConflict(
                platform_runtime_contracts::ConflictCode::SessionRevisionConflict,
                "Session revision changed. Reload context before committing.",
            ));
        }
        let mut message_ids = Vec::new();
        for m in &request.messages {
            let value = serde_json::to_value(&m.message).map_err(|_| RuntimeError::StoredData)?;
            let message_id = m.message_id;
            sqlx::query(
                "insert into messages(id,project_id,origin_run_id,message) values($1,$2,$3,$4)",
            )
            .bind(message_id)
            .bind(r.project_id)
            .bind(id)
            .bind(value)
            .execute(&mut *tx)
            .await?;
            sqlx::query("insert into session_messages(project_id,session_id,message_id,run_id) values($1,$2,$3,$4)").bind(r.project_id).bind(r.session_id).bind(message_id).bind(id).execute(&mut *tx).await?;
            message_ids.push(message_id);
        }
        for result in &request.input_results {
            let status = match result.status {
                InputStatus::Handled => "handled",
                InputStatus::Rejected => "rejected",
            };
            let changed=sqlx::query("update run_inputs set status=$3,handling=$4,handled_at=clock_timestamp() where id=$1 and run_id=$2 and status='pending'").bind(result.id).bind(id).bind(status).bind(json!(result.handling)).execute(&mut *tx).await?.rows_affected();
            if changed != 1 {
                return Err(RuntimeError::CodedConflict(
                    platform_runtime_contracts::ConflictCode::InputStateConflict,
                    "Input is missing, belongs to another run, or is already settled.",
                ));
            }
        }
        if let Some(c) = &request.checkpoint {
            let current: Option<i64> =
                sqlx::query_scalar("select version from run_checkpoints where run_id=$1")
                    .bind(id)
                    .fetch_optional(&mut *tx)
                    .await?;
            if current.unwrap_or(0) != c.expected_version {
                return Err(RuntimeError::CodedConflict(
                    platform_runtime_contracts::ConflictCode::CheckpointVersionConflict,
                    "Checkpoint version changed.",
                ));
            }
            if current.is_some() {
                sqlx::query("update run_checkpoints set version=version+1,state=$2,saved_by_lease_epoch=$3 where run_id=$1").bind(id).bind(json!(c.state)).bind(owner.lease_epoch).execute(&mut *tx).await?;
            } else {
                sqlx::query("insert into run_checkpoints(run_id,state,saved_by_lease_epoch) values($1,$2,$3)").bind(id).bind(json!(c.state)).bind(owner.lease_epoch).execute(&mut *tx).await?;
            }
        }
        let session_state = super::session_state::apply(
            &mut tx,
            r.project_id,
            r.session_id,
            id,
            owner,
            &request.session_state,
        )
        .await?;
        for wait in &request.cancel_wait_ids {
            let changed=sqlx::query("update run_waits set status='cancelled',resolved_at=clock_timestamp(),result='{}' where id=$1 and run_id=$2 and status='pending'").bind(wait).bind(id).execute(&mut *tx).await?.rows_affected();
            if changed != 1 {
                return Err(RuntimeError::CodedConflict(
                    platform_runtime_contracts::ConflictCode::WaitStateConflict,
                    "Wait is missing or already resolved.",
                ));
            }
        }
        let mut wait_ids = Vec::new();
        for wait in &request.waits {
            wait_ids.push(super::waits::insert(&mut tx, r.project_id, id, wait).await?);
        }
        let resolved = super::waits::resolve(&mut tx, id).await?;
        let event_rows = append_events(&mut tx, id, &request.events).await?;
        let (mut status, mut available, final_message, error) = match &request.disposition {
            Disposition::Running => ("running", None, None, None),
            Disposition::Ready { available_at } => ("ready", *available_at, None, None),
            Disposition::Waiting => ("waiting", None, None, None),
            Disposition::Completed { final_message_id } => {
                ("completed", None, Some(*final_message_id), None)
            }
            Disposition::Failed { error } => ("failed", None, None, Some(json!(error))),
            Disposition::Aborted => ("aborted", None, None, None),
        };
        if status == "aborted" && r.abort_requested_at.is_none() {
            return Err(RuntimeError::CodedConflict(
                platform_runtime_contracts::ConflictCode::RunStateConflict,
                "Only an abort-requested run can acknowledge abort. Use failed for harness errors.",
            ));
        }
        // Check new wake-ups under the same run lock used by input delivery.
        // This closes the input-arrives-before-park race without prescribing when
        // the harness consumes that input. Settled inputs do not keep it awake.
        let pending: bool = sqlx::query_scalar(
            "select exists(select 1 from run_inputs where run_id=$1 and status='pending')",
        )
        .bind(id)
        .fetch_one(&mut *tx)
        .await?;
        if status == "completed" && pending {
            return Err(RuntimeError::CodedConflict(
                platform_runtime_contracts::ConflictCode::InputStateConflict,
                "Settle pending inputs before completing the run. New input may have arrived while the model was finishing.",
            ));
        }
        if status == "ready" && (pending || resolved) {
            available = None;
        }
        if status == "waiting" {
            let has_wait: bool = sqlx::query_scalar(
                "select exists(select 1 from run_waits where run_id=$1 and status='pending')",
            )
            .bind(id)
            .fetch_one(&mut *tx)
            .await?;
            if pending || resolved {
                status = "ready";
            } else if !has_wait {
                return Err(RuntimeError::Invalid(
                    "Waiting requires a pending durable wait.",
                ));
            }
        }
        let terminal = ["completed", "failed", "aborted"].contains(&status);
        if terminal {
            sqlx::query("update run_waits set status='cancelled',resolved_at=clock_timestamp(),result=jsonb_build_object('reason','run_terminated') where run_id=$1 and status='pending'").bind(id).execute(&mut *tx).await?;
        }
        owned(&mut tx, id, owner).await?;
        sqlx::query("update runs set status=$2,version=version+1,available_at=case when $2='ready' then coalesce($3,clock_timestamp()) else null end,worker_id=case when $2='running' then worker_id else null end,lease_expires_at=case when $2='running' then lease_expires_at else null end,final_message_id=$4,error=$5,finished_at=case when $6 then clock_timestamp() else null end where id=$1")
            .bind(id).bind(status).bind(available).bind(final_message).bind(error).bind(terminal).execute(&mut *tx).await?;
        if status != "running" {
            let kind = match status {
                "ready" => "run.yielded",
                "waiting" => "run.waiting",
                "completed" => "run.completed",
                "failed" => "run.failed",
                _ => "run.aborted",
            };
            mutations::event(
                &mut tx,
                id,
                kind,
                json!({"final_message_id":final_message,"wait_ids":wait_ids}),
            )
            .await?;
        }
        mutations::event(&mut tx,id,"run.committed",json!({"message_ids":message_ids,"input_ids":request.input_results.iter().map(|r|r.id).collect::<Vec<_>>()})).await?;
        mutations::activity(&mut tx, r.session_id).await?;
        mutations::hint(&mut tx, id).await?;
        let checkpoint: Option<Value> =
            sqlx::query_scalar("select to_jsonb(c) from run_checkpoints c where run_id=$1")
                .bind(id)
                .fetch_optional(&mut *tx)
                .await?;
        let revision: i64 = sqlx::query_scalar("select current_revision from sessions where id=$1")
            .bind(r.session_id)
            .fetch_one(&mut *tx)
            .await?;
        let reply=Reply::new(200,json!({"run":run_json(&mut tx,id).await?,"session_revision":revision,"checkpoint":checkpoint,"session_state":session_state,"message_ids":message_ids,"wait_ids":wait_ids,"events":event_rows})).owned_by(owner);
        receipts::save(&mut tx, r.project_id, scope, id, key, &hash, &reply).await?;
        tx.commit().await?;
        self.notify(id);
        Ok(reply)
    }
}
