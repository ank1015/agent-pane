use super::{
    RuntimeService, configuration,
    error::{Result, RuntimeError},
    model::*,
    queries::{run_json, session_json},
    receipts::{self, Reply},
};
use serde_json::{Value, json};
use sqlx::{Postgres, Transaction};
use uuid::Uuid;

type Tx<'a> = Transaction<'a, Postgres>;
pub(super) async fn session(tx: &mut Tx<'_>, id: Uuid) -> Result<SessionRow> {
    sqlx::query_as("select id,project_id,harness_id,config,title,current_revision,archived_at from sessions where id=$1 for update").bind(id).fetch_optional(&mut **tx).await?.ok_or(RuntimeError::NotFound)
}
pub(super) async fn run(tx: &mut Tx<'_>, id: Uuid) -> Result<RunRow> {
    sqlx::query_as("select id,project_id,session_id,status,abort_requested_at from runs where id=$1 for update").bind(id).fetch_optional(&mut **tx).await?.ok_or(RuntimeError::NotFound)
}
pub(super) async fn enabled(tx: &mut Tx<'_>, project: Uuid, id: &str) -> Result<()> {
    super::project_harnesses::require_enabled(tx, project, id).await
}
pub(super) async fn event(tx: &mut Tx<'_>, id: Uuid, kind: &str, payload: Value) -> Result<()> {
    sqlx::query(
        "insert into run_events(id,run_id,type,source,payload) values($1,$2,$3,'runtime',$4)",
    )
    .bind(Uuid::now_v7())
    .bind(id)
    .bind(kind)
    .bind(payload)
    .execute(&mut **tx)
    .await?;
    Ok(())
}
pub(super) async fn hint(tx: &mut Tx<'_>, id: Uuid) -> Result<()> {
    sqlx::query("select pg_notify('platform_runtime_run',$1)")
        .bind(id.to_string())
        .execute(&mut **tx)
        .await?;
    Ok(())
}
pub(super) async fn activity(tx: &mut Tx<'_>, id: Uuid) -> Result<()> {
    sqlx::query("update sessions set last_activity_at=greatest(last_activity_at,clock_timestamp()) where id=$1").bind(id).execute(&mut **tx).await?;
    Ok(())
}
pub(super) async fn input(
    tx: &mut Tx<'_>,
    project: Uuid,
    run: Uuid,
    kind: &str,
    payload: Value,
    key: &str,
) -> Result<Value> {
    Ok(sqlx::query_scalar("insert into run_inputs(id,project_id,run_id,kind,payload,deduplication_key) values($1,$2,$3,$4,$5,$6) returning to_jsonb(run_inputs) - 'deduplication_key'")
        .bind(Uuid::now_v7()).bind(project).bind(run).bind(kind).bind(payload).bind(key).fetch_one(&mut **tx).await?)
}
async fn start(tx: &mut Tx<'_>, s: &SessionRow, request: &StartRun) -> Result<(Uuid, Value)> {
    start_with_parent(tx, s, request, None).await
}
/// Caller holds the session/run locks and decides whether terminal targets are
/// errors or no-ops. Repeated requests reuse the original abort input.
pub(super) async fn request_abort(
    tx: &mut Tx<'_>,
    run: &RunRow,
    reason: Option<&str>,
    source: Option<Uuid>,
) -> Result<Value> {
    if run.abort_requested_at.is_some() {
        return Ok(sqlx::query_scalar("select to_jsonb(i)-'deduplication_key' from run_inputs i where run_id=$1 and deduplication_key='runtime:abort'")
            .bind(run.id).fetch_optional(&mut **tx).await?.unwrap_or(Value::Null));
    }
    let input: Value = sqlx::query_scalar("insert into run_inputs(id,project_id,run_id,kind,source_run_id,deduplication_key,payload) values($1,$2,$3,'abort',$4,'runtime:abort',$5) returning to_jsonb(run_inputs)-'deduplication_key'")
        .bind(Uuid::now_v7()).bind(run.project_id).bind(run.id).bind(source).bind(json!({"reason":reason})).fetch_one(&mut **tx).await?;
    sqlx::query(
        "update runs set abort_requested_at=clock_timestamp(),version=version+1 where id=$1",
    )
    .bind(run.id)
    .execute(&mut **tx)
    .await?;
    let mut payload = json!({"input_id":input["id"]});
    if let Some(source) = source {
        payload["source_run_id"] = json!(source);
    }
    event(tx, run.id, "run.abort_requested", payload).await?;
    wake(tx, run.id, "abort").await?;
    activity(tx, run.session_id).await?;
    Ok(input)
}
pub(super) async fn start_with_parent(
    tx: &mut Tx<'_>,
    s: &SessionRow,
    request: &StartRun,
    parent: Option<Uuid>,
) -> Result<(Uuid, Value)> {
    if s.archived_at.is_some() {
        return Err(RuntimeError::CodedConflict(
            platform_runtime_contracts::ConflictCode::SessionStateConflict,
            "Unarchive the session before starting a run.",
        ));
    }
    if s.current_revision != request.expected_session_revision {
        return Err(RuntimeError::CodedConflict(
            platform_runtime_contracts::ConflictCode::SessionRevisionConflict,
            "Session history changed. Reload before starting a run.",
        ));
    }
    let live:bool=sqlx::query_scalar("select exists(select 1 from runs where session_id=$1 and status in ('ready','running','waiting'))").bind(s.id).fetch_one(&mut **tx).await?;
    if live {
        return Err(RuntimeError::CodedConflict(
            platform_runtime_contracts::ConflictCode::SessionStateConflict,
            "This session already has an active run. Submit input to that run instead.",
        ));
    }
    enabled(tx, s.project_id, &s.harness_id).await?;
    let config = &s.config;
    let id = Uuid::now_v7();
    sqlx::query(
        "insert into runs(id,project_id,session_id,config,parent_run_id) values($1,$2,$3,$4,$5)",
    )
    .bind(id)
    .bind(s.project_id)
    .bind(s.id)
    .bind(config)
    .bind(parent)
    .execute(&mut **tx)
    .await?;
    let initial = input(
        tx,
        s.project_id,
        id,
        "user_message",
        json!({"message":request.input}),
        "initial_input",
    )
    .await?;
    event(tx, id, "run.created", json!({"input_id":initial["id"]})).await?;
    activity(tx, s.id).await?;
    hint(tx, id).await?;
    Ok((id, initial))
}
pub(super) async fn wake(tx: &mut Tx<'_>, id: Uuid, reason: &str) -> Result<()> {
    let changed=sqlx::query("update runs set status='ready',available_at=clock_timestamp(),version=version+1 where id=$1 and (status='waiting' or (status='ready' and available_at>clock_timestamp()))").bind(id).execute(&mut **tx).await?.rows_affected();
    if changed > 0 {
        event(tx, id, "run.woken", json!({"reason":reason})).await?;
    }
    hint(tx, id).await
}
pub(super) async fn satisfy_inputs(
    tx: &mut Tx<'_>,
    id: Uuid,
    kind: &str,
    payload: &Value,
    input_id: &Value,
) -> Result<()> {
    // Run ownership is locked by the caller. A committed input is durable even
    // when no wait matches it; only the harness decides when it reaches a model.
    sqlx::query("update run_wait_dependencies d set satisfied_at=clock_timestamp(),result=$3 from run_waits w where d.wait_id=w.id and w.run_id=$1 and w.status='pending' and d.kind='input' and d.satisfied_at is null and d.input_kind=$2 and (d.correlation_key is null or d.correlation_key=$4)")
        .bind(id).bind(kind).bind(json!({"input_id":input_id,"payload":payload})).bind(payload.get("correlation_key").and_then(Value::as_str)).execute(&mut **tx).await?;
    // waits::resolve owns settlement, result aggregation, versioning and events
    // for both delivery-before-registration and registration-before-delivery.
    Ok(())
}

impl RuntimeService {
    pub(super) async fn transaction(&self) -> Result<Tx<'_>> {
        let mut tx = self.pool.begin().await?;
        sqlx::query("select set_config('lock_timeout','5s',true),set_config('statement_timeout','30s',true)").execute(&mut *tx).await?;
        Ok(tx)
    }
    pub(super) async fn create_session(
        &self,
        project: Uuid,
        key: &str,
        request: CreateSession,
    ) -> Result<Reply> {
        validate_title(request.title.as_deref())?;
        if let Some(r) = &request.initial_run {
            r.validate()?;
        }
        let hash = receipts::hash(&request)?;
        let scope = "project.create_session";
        let mut tx = self.transaction().await?;
        if let Some(reply) = receipts::replay(&mut tx, scope, project, key, &hash).await? {
            return Ok(reply);
        }
        let exists: Option<Uuid> =
            sqlx::query_scalar("select project_id from projects where project_id=$1 for key share")
                .bind(project)
                .fetch_optional(&mut *tx)
                .await?;
        exists.ok_or(RuntimeError::NotFound)?;
        enabled(&mut tx, project, &request.harness_id).await?;
        let config =
            configuration::resolve(&mut tx, &request.harness_id, &request.config_override).await?;
        let id = Uuid::now_v7();
        sqlx::query(
            "insert into sessions(id,project_id,harness_id,title,config) values($1,$2,$3,$4,$5)",
        )
        .bind(id)
        .bind(project)
        .bind(request.harness_id)
        .bind(request.title)
        .bind(config)
        .execute(&mut *tx)
        .await?;
        let mut run_id = None;
        let mut initial = Value::Null;
        if let Some(r) = request.initial_run {
            let s = session(&mut tx, id).await?;
            let (id, i) = start(&mut tx, &s, &r).await?;
            run_id = Some(id);
            initial = i;
        }
        let r = match run_id {
            Some(id) => run_json(&mut tx, id).await?,
            None => Value::Null,
        };
        let reply = Reply::new(
            201,
            json!({"session":session_json(&mut tx,id).await?,"run":r,"input":initial}),
        );
        receipts::save(&mut tx, project, scope, project, key, &hash, &reply).await?;
        tx.commit().await?;
        if let Some(id) = run_id {
            self.notify(id);
        }
        Ok(reply)
    }
    pub(super) async fn fork(&self, id: Uuid, key: &str, request: ForkSession) -> Result<Reply> {
        validate_title(request.title.as_deref())?;
        if request.at_revision < 0 {
            return Err(RuntimeError::Invalid("at_revision must be nonnegative."));
        }
        if let Some(r) = &request.initial_run {
            r.validate()?;
        }
        let hash = receipts::hash(&request)?;
        let scope = "session.fork";
        let mut tx = self.transaction().await?;
        if let Some(reply) = receipts::replay(&mut tx, scope, id, key, &hash).await? {
            return Ok(reply);
        }
        let source = session(&mut tx, id).await?;
        if request.at_revision > source.current_revision {
            return Err(RuntimeError::CodedConflict(
                platform_runtime_contracts::ConflictCode::SessionRevisionConflict,
                "The fork revision does not exist.",
            ));
        }
        let harness = request.harness_id.as_deref().unwrap_or(&source.harness_id);
        enabled(&mut tx, source.project_id, harness).await?;
        let config = configuration::resolve_from(
            &mut tx,
            harness,
            &request.config_override,
            (harness == source.harness_id).then_some(&source.config),
        )
        .await?;
        let child = Uuid::now_v7();
        sqlx::query("insert into sessions(id,project_id,harness_id,title,forked_from_session_id,forked_at_revision,config) values($1,$2,$3,$4,$5,$6,$7)")
            .bind(child).bind(source.project_id).bind(harness).bind(request.title.as_ref().or(source.title.as_ref())).bind(id).bind(request.at_revision).bind(config).execute(&mut *tx).await?;
        sqlx::query("insert into session_messages(project_id,session_id,revision,message_id,run_id) select project_id,$1,revision,message_id,null from session_messages where session_id=$2 and revision<=$3 order by revision")
            .bind(child).bind(id).bind(request.at_revision).execute(&mut *tx).await?;
        let mut run_id = None;
        let mut initial = Value::Null;
        if let Some(r) = request.initial_run {
            let s = session(&mut tx, child).await?;
            let (id, i) = start(&mut tx, &s, &r).await?;
            run_id = Some(id);
            initial = i;
        }
        let r = match run_id {
            Some(id) => run_json(&mut tx, id).await?,
            None => Value::Null,
        };
        let reply = Reply::new(
            201,
            json!({"session":session_json(&mut tx,child).await?,"run":r,"input":initial}),
        );
        receipts::save(&mut tx, source.project_id, scope, id, key, &hash, &reply).await?;
        tx.commit().await?;
        if let Some(id) = run_id {
            self.notify(id);
        }
        Ok(reply)
    }
    pub(super) async fn patch_session(&self, id: Uuid, request: PatchSession) -> Result<Value> {
        if request.title.is_none() && request.archived.is_none() {
            return Err(RuntimeError::Invalid("Provide title or archived."));
        }
        validate_title(request.title.as_ref().and_then(|t| t.as_deref()))?;
        let mut tx = self.transaction().await?;
        session(&mut tx, id).await?;
        if let Some(title) = request.title {
            sqlx::query("update sessions set title=$2 where id=$1")
                .bind(id)
                .bind(title)
                .execute(&mut *tx)
                .await?;
        }
        if let Some(archived) = request.archived {
            sqlx::query("update sessions set archived_at=case when $2 then coalesce(archived_at,clock_timestamp()) else null end where id=$1").bind(id).bind(archived).execute(&mut *tx).await?;
        }
        let result = session_json(&mut tx, id).await?;
        tx.commit().await?;
        Ok(result)
    }
    pub(super) async fn start_run(&self, id: Uuid, key: &str, request: StartRun) -> Result<Reply> {
        request.validate()?;
        let hash = receipts::hash(&request)?;
        let scope = "session.start_run";
        let mut tx = self.transaction().await?;
        if let Some(reply) = receipts::replay(&mut tx, scope, id, key, &hash).await? {
            return Ok(reply);
        }
        let s = session(&mut tx, id).await?;
        let (run, initial) = start(&mut tx, &s, &request).await?;
        let reply = Reply::new(
            201,
            json!({"run":run_json(&mut tx,run).await?,"input":initial}),
        );
        receipts::save(&mut tx, s.project_id, scope, id, key, &hash, &reply).await?;
        tx.commit().await?;
        self.notify(run);
        Ok(reply)
    }
    pub(super) async fn submit_input(
        &self,
        id: Uuid,
        key: &str,
        request: SubmitInput,
    ) -> Result<Reply> {
        let (kind, payload) = request.parts()?;
        let hash = receipts::hash(&request)?;
        let scope = "run.submit_input";
        let mut tx = self.transaction().await?;
        if let Some(reply) = receipts::replay(&mut tx, scope, id, key, &hash).await? {
            return Ok(reply);
        }
        // Lock session before run, matching run creation and transcript commits.
        let session_id: Uuid = sqlx::query_scalar("select session_id from runs where id=$1")
            .bind(id)
            .fetch_optional(&mut *tx)
            .await?
            .ok_or(RuntimeError::NotFound)?;
        session(&mut tx, session_id).await?;
        let r = run(&mut tx, id).await?;
        r.live()?;
        let key_hash = receipts::hash(&(scope, id, key))?;
        let i = input(&mut tx, r.project_id, id, kind, payload.clone(), &key_hash).await?;
        satisfy_inputs(&mut tx, id, kind, &payload, &i["id"]).await?;
        super::waits::resolve(&mut tx, id).await?;
        event(
            &mut tx,
            id,
            "run.input_received",
            json!({"input_id":i["id"],"kind":kind}),
        )
        .await?;
        wake(&mut tx, id, kind).await?;
        activity(&mut tx, r.session_id).await?;
        let reply = Reply::new(201, json!({"input":i,"run":run_json(&mut tx,id).await?}));
        receipts::save(&mut tx, r.project_id, scope, id, key, &hash, &reply).await?;
        tx.commit().await?;
        self.notify(id);
        Ok(reply)
    }
    pub(super) async fn abort(&self, id: Uuid, key: &str, request: AbortRun) -> Result<Reply> {
        if let Some(reason) = &request.reason {
            nonempty(
                reason,
                2048,
                "Abort reasons must contain 1–2048 characters without surrounding whitespace.",
            )?;
        }
        let hash = receipts::hash(&request)?;
        let scope = "run.abort";
        let mut tx = self.transaction().await?;
        if let Some(reply) = receipts::replay(&mut tx, scope, id, key, &hash).await? {
            return Ok(reply);
        }
        let session_id: Uuid = sqlx::query_scalar("select session_id from runs where id=$1")
            .bind(id)
            .fetch_optional(&mut *tx)
            .await?
            .ok_or(RuntimeError::NotFound)?;
        session(&mut tx, session_id).await?;
        let r = run(&mut tx, id).await?;
        r.live()?;
        let i = request_abort(&mut tx, &r, request.reason.as_deref(), None).await?;
        let reply = Reply::new(202, json!({"run":run_json(&mut tx,id).await?,"input":i}));
        receipts::save(&mut tx, r.project_id, scope, id, key, &hash, &reply).await?;
        tx.commit().await?;
        self.notify(id);
        Ok(reply)
    }
}
