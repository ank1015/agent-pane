//! Cross-run effects are ordinary durable inputs and run records. They neither
//! execute harnesses inline nor impose parent/child cancellation or join policy.
use super::{
    RuntimeService,
    error::{Result, RuntimeError},
    model::{ValidateStartRun, nonempty, validate_title},
    mutations,
    queries::{run_json, session_json},
    receipts::{self, Reply},
    worker_model::*,
    workers::{lock_runs, owned},
};
use serde_json::{Value, json};
use uuid::Uuid;

impl RuntimeService {
    pub(super) async fn follow_up(
        &self,
        source: Uuid,
        owner: Owner,
        key: &str,
        request: FollowUp,
    ) -> Result<Reply> {
        request.run.validate()?;
        let scope = "run.follow_up";
        let hash = receipts::hash(&request)?;
        let mut tx = self.transaction().await?;
        if let Some(reply) =
            receipts::worker_replay(&mut tx, scope, source, key, &hash, owner).await?
        {
            return Ok(reply);
        }
        // Lock both sessions before the source run, using the same ordering as
        // cross-run operations. The target session lock serializes new runs.
        let source_session: Uuid = sqlx::query_scalar("select session_id from runs where id=$1")
            .bind(source)
            .fetch_optional(&mut *tx)
            .await?
            .ok_or(RuntimeError::NotFound)?;
        let mut sessions = vec![source_session, request.target_session_id];
        sessions.sort();
        sessions.dedup();
        for id in sessions {
            mutations::session(&mut tx, id).await?;
        }
        let from = mutations::run(&mut tx, source).await?;
        owned(&mut tx, source, owner).await?;
        let session = mutations::session(&mut tx, request.target_session_id).await?;
        if from.project_id != session.project_id {
            return Err(RuntimeError::NotFound);
        }
        mutations::enabled(&mut tx, &session.harness_id).await?;
        let (run, input) =
            mutations::start_with_parent(&mut tx, &session, &request.run, Some(source)).await?;
        mutations::event(
            &mut tx,
            source,
            "run.follow_up_created",
            json!({"run_id":run,"session_id":session.id}),
        )
        .await?;
        owned(&mut tx, source, owner).await?;
        mutations::hint(&mut tx, source).await?;
        let reply = Reply::new(201, json!({"session":session_json(&mut tx,session.id).await?,"run":run_json(&mut tx,run).await?,"input":input})).owned_by(owner);
        receipts::save(&mut tx, from.project_id, scope, source, key, &hash, &reply).await?;
        tx.commit().await?;
        self.notify(source);
        self.notify(run);
        Ok(reply)
    }
    pub(super) async fn child(
        &self,
        parent: Uuid,
        owner: Owner,
        key: &str,
        request: Child,
    ) -> Result<Reply> {
        validate_title(request.title.as_deref())?;
        request.initial_run.validate()?;
        if request.fork_at_revision.is_some_and(|r| r < 0) {
            return Err(RuntimeError::Invalid("Fork revision must be nonnegative."));
        }
        let scope = "run.child";
        let hash = receipts::hash(&request)?;
        let mut tx = self.transaction().await?;
        if let Some(reply) =
            receipts::worker_replay(&mut tx, scope, parent, key, &hash, owner).await?
        {
            return Ok(reply);
        }
        lock_runs(&mut tx, &[parent]).await?;
        owned(&mut tx, parent, owner).await?;
        let p = mutations::run(&mut tx, parent).await?;
        let source = mutations::session(&mut tx, p.session_id).await?;
        if request
            .fork_at_revision
            .is_some_and(|r| r > source.current_revision)
        {
            return Err(RuntimeError::CodedConflict(
                platform_runtime_contracts::ConflictCode::SessionRevisionConflict,
                "The fork revision does not exist.",
            ));
        }
        let harness = request.harness_id.as_deref().unwrap_or(&source.harness_id);
        mutations::enabled(&mut tx, harness).await?;
        let session = Uuid::now_v7();
        sqlx::query("insert into sessions(id,project_id,harness_id,title,forked_from_session_id,forked_at_revision) values($1,$2,$3,$4,$5,$6)").bind(session).bind(p.project_id).bind(harness).bind(request.title).bind(request.fork_at_revision.map(|_|source.id)).bind(request.fork_at_revision).execute(&mut *tx).await?;
        if let Some(revision) = request.fork_at_revision {
            sqlx::query("insert into session_messages(project_id,session_id,revision,message_id,run_id) select project_id,$1,revision,message_id,null from session_messages where session_id=$2 and revision<=$3 order by revision")
                .bind(session).bind(source.id).bind(revision).execute(&mut *tx).await?;
        }
        let s = mutations::session(&mut tx, session).await?;
        let (child, input) =
            mutations::start_with_parent(&mut tx, &s, &request.initial_run, Some(parent)).await?;
        mutations::event(
            &mut tx,
            parent,
            "run.child_created",
            json!({"run_id":child,"session_id":session}),
        )
        .await?;
        owned(&mut tx, parent, owner).await?;
        mutations::hint(&mut tx, parent).await?;
        let reply=Reply::new(201,json!({"session":session_json(&mut tx,session).await?,"run":run_json(&mut tx,child).await?,"input":input})).owned_by(owner);
        receipts::save(&mut tx, p.project_id, scope, parent, key, &hash, &reply).await?;
        tx.commit().await?;
        self.notify(parent);
        self.notify(child);
        Ok(reply)
    }
    pub(super) async fn run_message(
        &self,
        source: Uuid,
        owner: Owner,
        key: &str,
        request: RunMessage,
    ) -> Result<Reply> {
        nonempty(&request.kind, 128, "Provide a valid message kind.")?;
        // User and approval inputs belong to the Application API; abort must set
        // both its marker and input through the dedicated operation below.
        if !["agent_message", "operation_result"].contains(&request.kind.as_str()) {
            return Err(RuntimeError::Invalid(
                "Cross-run message kind must be agent_message or operation_result.",
            ));
        }
        if request.kind == "operation_result" {
            nonempty(
                request
                    .payload
                    .get("correlation_key")
                    .and_then(Value::as_str)
                    .unwrap_or(""),
                256,
                "Operation results require a correlation_key.",
            )?;
        }
        let scope = "run.message";
        let hash = receipts::hash(&request)?;
        let mut tx = self.transaction().await?;
        if let Some(reply) =
            receipts::worker_replay(&mut tx, scope, source, key, &hash, owner).await?
        {
            return Ok(reply);
        }
        let target = request.target_run_id;
        lock_runs(&mut tx, &[source, target]).await?;
        owned(&mut tx, source, owner).await?;
        let from = mutations::run(&mut tx, source).await?;
        let to = mutations::run(&mut tx, target).await?;
        if from.project_id != to.project_id {
            return Err(RuntimeError::NotFound);
        }
        to.live()?;
        let dedup = receipts::hash(&(scope, source, key))?;
        let input:Value=sqlx::query_scalar("insert into run_inputs(id,project_id,run_id,kind,source_run_id,deduplication_key,payload) values($1,$2,$3,$4,$5,$6,$7) returning to_jsonb(run_inputs)-'deduplication_key'")
            .bind(Uuid::now_v7()).bind(to.project_id).bind(target).bind(&request.kind).bind(source).bind(dedup).bind(json!(request.payload)).fetch_one(&mut *tx).await?;
        mutations::satisfy_inputs(
            &mut tx,
            target,
            &request.kind,
            &json!(request.payload),
            &input["id"],
        )
        .await?;
        super::waits::resolve(&mut tx, target).await?;
        mutations::wake(&mut tx, target, &request.kind).await?;
        mutations::event(
            &mut tx,
            target,
            "run.input_received",
            json!({"input_id":input["id"],"source_run_id":source,"kind":request.kind}),
        )
        .await?;
        mutations::activity(&mut tx, to.session_id).await?;
        owned(&mut tx, source, owner).await?;
        let reply = Reply::new(
            201,
            json!({"input":input,"run":run_json(&mut tx,target).await?}),
        )
        .owned_by(owner);
        receipts::save(&mut tx, from.project_id, scope, source, key, &hash, &reply).await?;
        tx.commit().await?;
        self.notify(target);
        Ok(reply)
    }
    pub(super) async fn abort_request(
        &self,
        source: Uuid,
        owner: Owner,
        key: &str,
        request: AbortRequest,
    ) -> Result<Reply> {
        if let Some(reason) = &request.reason {
            nonempty(
                reason,
                2048,
                "Provide an abort reason of 1–2048 characters.",
            )?;
        }
        let scope = "run.abort_request";
        let hash = receipts::hash(&request)?;
        let mut tx = self.transaction().await?;
        if let Some(reply) =
            receipts::worker_replay(&mut tx, scope, source, key, &hash, owner).await?
        {
            return Ok(reply);
        }
        let target = request.target_run_id;
        lock_runs(&mut tx, &[source, target]).await?;
        owned(&mut tx, source, owner).await?;
        let from = mutations::run(&mut tx, source).await?;
        let to = mutations::run(&mut tx, target).await?;
        if from.project_id != to.project_id {
            return Err(RuntimeError::NotFound);
        }
        // Terminal targets need no action; this makes parent cleanup race-safe.
        let mut input = Value::Null;
        if to.live().is_ok() {
            input = mutations::request_abort(&mut tx, &to, request.reason.as_deref(), Some(source))
                .await?;
        }
        owned(&mut tx, source, owner).await?;
        let reply = Reply::new(
            202,
            json!({"run":run_json(&mut tx,target).await?,"input":input}),
        )
        .owned_by(owner);
        receipts::save(&mut tx, from.project_id, scope, source, key, &hash, &reply).await?;
        tx.commit().await?;
        self.notify(target);
        Ok(reply)
    }
}
