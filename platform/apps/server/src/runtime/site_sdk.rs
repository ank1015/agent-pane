//! High-level site capabilities. Every mutation uses the existing runtime's
//! transaction, receipts, run/input primitives and admission checks.
use super::{
    RuntimeService, configuration,
    error::{Result, RuntimeError},
    model::*,
    mutations as m, queries as q, receipts,
};
use crate::providers::ProviderService;
use serde::{Deserialize, de::DeserializeOwned};
use serde_json::{Value, json};
use sqlx::{Postgres, Transaction};
use uuid::Uuid;

#[derive(Clone, Copy)]
pub struct SiteScope {
    pub site: Uuid,
    pub project: Uuid,
    pub invocation: Uuid,
    pub release: Uuid,
}
fn decode<T: DeserializeOwned>(value: Value) -> Result<T> {
    serde_json::from_value(value).map_err(|_| {
        RuntimeError::Invalid("Invalid capability arguments; unknown fields are not supported.")
    })
}
#[derive(Default, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
struct Options {
    limit: Option<u32>,
    cursor: Option<String>,
    after_revision: Option<i64>,
    run_id: Option<Uuid>,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
struct Read {
    session_id: Uuid,
    #[serde(default)]
    options: Options,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
struct HarnessId {
    harness_id: String,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
struct MutationOptions {
    idempotency_key: String,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Mutation<T> {
    input: T,
    options: MutationOptions,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
struct Start {
    harness_id: String,
    environment_id: Option<Uuid>,
    prompt: String,
    title: Option<String>,
    model: Model,
    account_id: Uuid,
    on_complete: Option<Completion>,
    #[serde(default)]
    options: Controls,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Model {
    provider: String,
    id: String,
}
#[derive(Default, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
struct Controls {
    reasoning_level: Option<String>,
    web_search_enabled: Option<bool>,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
struct Send {
    session_id: Uuid,
    prompt: String,
    expected_revision: i64,
    expected_run_id: Option<Uuid>,
    on_complete: Option<Completion>,
    options: MutationOptions,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct Completion {
    pub(super) path: String,
    #[serde(default)]
    pub(super) payload: Value,
}
pub(super) async fn subscribe(
    tx: &mut Transaction<'_, Postgres>,
    scope: SiteScope,
    run: Uuid,
    callback: Option<Completion>,
    result: &mut Value,
) -> Result<()> {
    let Some(callback) = callback else {
        return Ok(());
    };
    if !callback.path.starts_with('/')
        || callback.path.starts_with("//")
        || callback.path.len() > 2048
        || callback.path.contains(['?', '#'])
        || callback.path.chars().any(char::is_control)
        || serde_json::to_vec(&callback.payload)
            .map_err(|_| RuntimeError::StoredData)?
            .len()
            > 16 * 1024
    {
        return Err(RuntimeError::Invalid(
            "Callback requires a local endpoint path and at most 16 KiB of JSON payload.",
        ));
    }
    let id = Uuid::now_v7();
    sqlx::query("insert into site_callbacks(id,site_id,run_id,invocation_id,release_id,path,payload) values($1,$2,$3,$4,$5,$6,$7)")
        .bind(id).bind(scope.site).bind(run).bind(scope.invocation).bind(scope.release)
        .bind(callback.path).bind(callback.payload).execute(&mut **tx).await?;
    result["callback"] = json!({"subscriptionId":id});
    Ok(())
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
struct Stop {
    session_id: Uuid,
    expected_run_id: Uuid,
    reason: Option<String>,
    options: MutationOptions,
}
pub(super) fn key(key: &str) -> Result<()> {
    if key.is_empty() || key.len() > 256 || !key.bytes().all(|c| c.is_ascii_graphic()) {
        Err(RuntimeError::Invalid(
            "Provide a stable idempotency key of 1–256 visible ASCII characters.",
        ))
    } else {
        Ok(())
    }
}
fn message(prompt: &str) -> Result<llm_contracts::Message> {
    if prompt.trim().is_empty() || prompt.len() > 64 * 1024 {
        return Err(RuntimeError::Invalid("Prompt must contain 1–65536 bytes."));
    }
    decode(
        json!({"role":"user","id":Uuid::now_v7().to_string(),"timestamp":chrono::Utc::now().timestamp_millis(),"content":[{"type":"text","content":prompt}]}),
    )
}
pub(super) async fn scope(tx: &mut Transaction<'_, Postgres>, s: SiteScope) -> Result<()> {
    let found:Option<Uuid>=sqlx::query_scalar("select s.id from project_sites s join site_project_access a on a.project_id=s.project_id where s.id=$1 and s.project_id=$2 and s.deleted_at is null and s.desired_status='ready' and a.enabled for share of s")
        .bind(s.site).bind(s.project).fetch_optional(&mut **tx).await?;
    found.ok_or(RuntimeError::NotFound)?;
    Ok(())
}
async fn session_scope(
    tx: &mut Transaction<'_, Postgres>,
    s: SiteScope,
    id: Uuid,
) -> Result<SessionRow> {
    let session = m::session(tx, id).await?;
    if session.project_id != s.project {
        return Err(RuntimeError::NotFound);
    }
    Ok(session)
}
pub(super) async fn harness(
    tx: &mut Transaction<'_, Postgres>,
    project: Uuid,
    id: &str,
) -> Result<Value> {
    super::project_harnesses::require_enabled(tx, project, id).await?;
    sqlx::query_scalar("select to_jsonb(h) from harnesses h where h.id=$1 for share of h")
        .bind(id)
        .fetch_one(&mut **tx)
        .await
        .map_err(Into::into)
}
async fn environment(tx: &mut Transaction<'_, Postgres>, project: Uuid, id: Uuid) -> Result<Value> {
    sqlx::query_scalar("select jsonb_build_object('type',type,'workspace_root',workspace_root,'path',path) || case when type='machine' then jsonb_build_object('machine_id',machine_id) else jsonb_build_object('snapshot_id',snapshot_id) end from project_environments where project_id=$1 and id=$2 for share")
        .bind(project).bind(id).fetch_optional(&mut **tx).await?.ok_or(RuntimeError::NotFound)
}
pub(super) async fn validate_existing(
    tx: &mut Transaction<'_, Postgres>,
    session: &SessionRow,
) -> Result<()> {
    let h = harness(tx, session.project_id, &session.harness_id).await?;
    super::capabilities::validate_frozen_inputs(tx, session).await?;
    if h["config_schema"]["properties"]
        .get("environment")
        .is_some()
        && !session.config["environment"].is_null()
    {
        let found:bool=sqlx::query_scalar("select exists(select 1 from project_environments where project_id=$1 and (jsonb_build_object('type',type,'workspace_root',workspace_root,'path',path) || case when type='machine' then jsonb_build_object('machine_id',machine_id) else jsonb_build_object('snapshot_id',snapshot_id) end)=$2)")
            .bind(session.project_id).bind(&session.config["environment"]).fetch_one(&mut **tx).await?;
        if !found {
            return Err(RuntimeError::Invalid(
                "Session environment is no longer authorized in this project.",
            ));
        }
    }
    Ok(())
}
async fn attributed(
    tx: &mut Transaction<'_, Postgres>,
    s: SiteScope,
    op: &str,
    key: &str,
    session: Uuid,
    run: Uuid,
) -> Result<()> {
    sqlx::query("insert into site_runtime_operations(site_id,invocation_id,operation,operation_key,session_id,run_id) values($1,$2,$3,$4,$5,$6)")
        .bind(s.site).bind(s.invocation).bind(op).bind(key).bind(session).bind(run).execute(&mut **tx).await?;
    Ok(())
}
async fn accepted(
    tx: &mut Transaction<'_, Postgres>,
    session: Uuid,
    run: Uuid,
    input: Value,
) -> Result<Value> {
    Ok(
        json!({"sessionId":session,"runId":run,"session":q::session_json(tx,session).await?,"run":q::run_json(tx,run).await?,"input":input}),
    )
}
impl RuntimeService {
    pub async fn site_capability(
        &self,
        s: SiteScope,
        method: &str,
        args: Value,
        providers: &ProviderService,
    ) -> Result<Value> {
        if super::capabilities::is_method(method) {
            return self
                .platform_capability(
                    super::capabilities::Caller::Site(s),
                    method,
                    args,
                    Some(providers),
                )
                .await;
        }
        // Reads also enforce the current access grant. Never use a guest-supplied project.
        let mut tx = self.transaction().await?;
        scope(&mut tx, s).await?;
        tx.commit().await?;
        match method {
            "callbacks.list" => {
                #[derive(Deserialize)]
                #[serde(deny_unknown_fields)]
                struct CallbackList {
                    #[serde(default)]
                    options: crate::sites::callbacks::List,
                }
                let a: CallbackList = decode(args)?;
                crate::sites::callbacks::list(&self.pool, s.site, a.options)
                    .await
                    .map_err(|e| match e {
                        crate::sites::Error::Invalid(m) => RuntimeError::Invalid(m),
                        crate::sites::Error::Database(e) => RuntimeError::Database(e),
                        _ => RuntimeError::NotFound,
                    })
            }
            "callbacks.get" => {
                #[derive(Deserialize)]
                #[serde(deny_unknown_fields, rename_all = "camelCase")]
                struct CallbackId {
                    callback_id: Uuid,
                }
                let a: CallbackId = decode(args)?;
                crate::sites::callbacks::get(&self.pool, s.site, a.callback_id)
                    .await
                    .map_err(|e| match e {
                        crate::sites::Error::Database(e) => RuntimeError::Database(e),
                        _ => RuntimeError::NotFound,
                    })
            }
            "sessions.startOptions" => {
                let a: HarnessId = decode(args)?;
                let mut tx = self.transaction().await?;
                let h = harness(&mut tx, s.project, &a.harness_id).await?;
                tx.commit().await?;
                let accounts = self.site_accounts(&h, providers).await?;
                let props = &h["config_schema"]["properties"];
                let environment_required = h["config_schema"]["required"]
                    .as_array()
                    .is_some_and(|required| required.contains(&json!("environment")));
                Ok(
                    json!({"harnessId":a.harness_id,"environmentRequired":environment_required,"accounts":accounts,"reasoningLevels":props["reasoning_level"]["enum"],"defaultReasoning":h["default_config"]["reasoning_level"],"webSearchSupported":props["web_search_enabled"]["type"]=="boolean","defaultWebSearch":h["default_config"]["web_search_enabled"]}),
                )
            }
            "sessions.start" => self.site_start(s, args, providers).await,
            "sessions.send" => self.site_send(s, args).await,
            "sessions.stop" => self.site_stop(s, args).await,
            "sessions.metrics" => {
                let a: Read = decode(args)?;
                let owner: Uuid = sqlx::query_scalar("select project_id from sessions where id=$1")
                    .bind(a.session_id)
                    .fetch_optional(&self.pool)
                    .await?
                    .ok_or(RuntimeError::NotFound)?;
                if owner != s.project {
                    return Err(RuntimeError::NotFound);
                }
                if let Some(run) = a.options.run_id {
                    let found: bool = sqlx::query_scalar(
                        "select exists(select 1 from runs where id=$1 and session_id=$2)",
                    )
                    .bind(run)
                    .bind(a.session_id)
                    .fetch_one(&self.pool)
                    .await?;
                    if !found {
                        return Err(RuntimeError::NotFound);
                    }
                }
                if a.options.limit.is_some()
                    || a.options.cursor.is_some()
                    || a.options.after_revision.is_some()
                {
                    return Err(RuntimeError::Invalid("Metrics accept runId only."));
                }
                self.site_metrics(a.session_id, a.options.run_id).await
            }

            _ => Err(RuntimeError::Invalid(
                "Unknown or unavailable Platform capability.",
            )),
        }
    }
    async fn site_accounts(&self, h: &Value, providers: &ProviderService) -> Result<Vec<Value>> {
        let accounts = providers.list_accounts().await.map_err(|_| {
            RuntimeError::Conflict("Provider inventory is unavailable. Retry later.")
        })?;
        let mut selected = vec![];
        for account in accounts {
            let a = serde_json::to_value(account).map_err(|_| RuntimeError::StoredData)?;
            let id: Uuid = decode(a["id"].clone())?;
            let provider = a["provider"].as_str().ok_or(RuntimeError::StoredData)?;
            let models = h["supported_models"][provider].as_array();
            if a["status"] == "enabled" && models.is_some_and(|m| !m.is_empty()) {
                selected.push(
                    json!({"accountId":id,"name":a["name"],"provider":provider,"modelIds":models}),
                );
            }
        }
        Ok(selected)
    }
    async fn site_start(
        &self,
        s: SiteScope,
        args: Value,
        providers: &ProviderService,
    ) -> Result<Value> {
        let a: Mutation<Start> = decode(args.clone())?;
        key(&a.options.idempotency_key)?;
        let op = "site.session.start";
        let hash = receipts::hash(&args)?;
        // Replay precedes mutable account/environment discovery. Successful retries
        // return the original IDs even after those resources change.
        let mut tx = self.transaction().await?;
        scope(&mut tx, s).await?;
        if let Some(r) =
            receipts::replay(&mut tx, op, s.site, &a.options.idempotency_key, &hash).await?
        {
            return Ok(r.body);
        }
        let h = harness(&mut tx, s.project, &a.input.harness_id).await?;
        tx.commit().await?;
        let accounts = self.site_accounts(&h, providers).await?;
        if !accounts.iter().any(|v| {
            v["accountId"] == a.input.account_id.to_string()
                && v["provider"] == a.input.model.provider
                && v["modelIds"]
                    .as_array()
                    .is_some_and(|ids| ids.contains(&json!(a.input.model.id)))
        }) {
            return Err(RuntimeError::Invalid(
                "Select an active account and supported model.",
            ));
        }
        let input = message(&a.input.prompt)?;
        validate_title(a.input.title.as_deref())?;
        let mut tx = self.transaction().await?;
        scope(&mut tx, s).await?;
        if let Some(r) =
            receipts::replay(&mut tx, op, s.site, &a.options.idempotency_key, &hash).await?
        {
            return Ok(r.body);
        }
        let h = harness(&mut tx, s.project, &a.input.harness_id).await?;
        if !h["supported_models"][&a.input.model.provider]
            .as_array()
            .is_some_and(|ids| ids.contains(&json!(a.input.model.id)))
        {
            return Err(RuntimeError::Configuration);
        }
        let mut patch = json!({"model":{"provider":a.input.model.provider,"id":a.input.model.id},"account_id":a.input.account_id});
        let environment_required = h["config_schema"]["required"]
            .as_array()
            .is_some_and(|required| required.contains(&json!("environment")));
        match (environment_required, a.input.environment_id) {
            (true, Some(id)) => patch["environment"] = environment(&mut tx, s.project, id).await?,
            (false, None) => {}
            _ => {
                return Err(RuntimeError::Invalid(
                    "Environment selection does not match harness requirements.",
                ));
            }
        }
        if let Some(value) = a.input.options.reasoning_level {
            patch["reasoning_level"] = json!(value);
        }
        if let Some(value) = a.input.options.web_search_enabled {
            if h["config_schema"]["properties"]["web_search_enabled"]["type"] != "boolean" {
                return Err(RuntimeError::Invalid("Web search option is not supported."));
            }
            patch["web_search_enabled"] = json!(value);
        }
        let config =
            configuration::resolve(&mut tx, &a.input.harness_id, patch.as_object().unwrap())
                .await?;
        super::run_outputs::validate_inputs(&mut tx, s.project, &a.input.harness_id, &config)
            .await?;
        let session = Uuid::now_v7();
        sqlx::query(
            "insert into sessions(id,project_id,harness_id,title,config) values($1,$2,$3,$4,$5)",
        )
        .bind(session)
        .bind(s.project)
        .bind(a.input.harness_id)
        .bind(a.input.title)
        .bind(config)
        .execute(&mut *tx)
        .await?;
        let row = m::session(&mut tx, session).await?;
        let (run, i) = m::start_with_parent(
            &mut tx,
            &row,
            &StartRun {
                expected_session_revision: 0,
                input,
            },
            None,
        )
        .await?;
        let mut result = accepted(&mut tx, session, run, i).await?;
        subscribe(&mut tx, s, run, a.input.on_complete, &mut result).await?;
        attributed(&mut tx, s, op, &a.options.idempotency_key, session, run).await?;
        receipts::save(
            &mut tx,
            s.project,
            op,
            s.site,
            &a.options.idempotency_key,
            &hash,
            &receipts::Reply::new(201, result.clone()),
        )
        .await?;
        tx.commit().await?;
        self.notify(run);
        Ok(result)
    }
    async fn site_send(&self, s: SiteScope, args: Value) -> Result<Value> {
        if args.get("expectedRunId").is_none() {
            return Err(RuntimeError::Invalid(
                "Provide the observed expectedRunId, or explicit null for an idle session.",
            ));
        }
        let a: Send = decode(args.clone())?;
        key(&a.options.idempotency_key)?;
        let op = "site.session.send";
        let hash = receipts::hash(&args)?;
        let input = message(&a.prompt)?;
        let mut tx = self.transaction().await?;
        scope(&mut tx, s).await?;
        if let Some(r) =
            receipts::replay(&mut tx, op, s.site, &a.options.idempotency_key, &hash).await?
        {
            return Ok(r.body);
        }
        let session = session_scope(&mut tx, s, a.session_id).await?;
        validate_existing(&mut tx, &session).await?;
        if session.current_revision != a.expected_revision {
            return Err(RuntimeError::Conflict(
                "Session revision changed. Reload before sending.",
            ));
        }
        let active: Option<Uuid> = sqlx::query_scalar(
            "select id from runs where session_id=$1 and status in ('ready','running','waiting')",
        )
        .bind(session.id)
        .fetch_optional(&mut *tx)
        .await?;
        if active != a.expected_run_id {
            return Err(RuntimeError::Conflict(
                "Active run changed. Reload before sending.",
            ));
        }
        if active.is_some() && a.on_complete.is_some() {
            return Err(RuntimeError::Invalid(
                "onComplete is supported when creating a new run, not when steering an active run.",
            ));
        }
        let (run, i) = if let Some(run) = active {
            let row = m::run(&mut tx, run).await?;
            row.live()?;
            let payload = json!({"message":input});
            let dedup = receipts::hash(&(s.site, op, &a.options.idempotency_key))?;
            let i = m::input(
                &mut tx,
                s.project,
                run,
                "user_message",
                payload.clone(),
                &dedup,
            )
            .await?;
            m::satisfy_inputs(&mut tx, run, "user_message", &payload, &i["id"]).await?;
            super::waits::resolve(&mut tx, run).await?;
            m::event(
                &mut tx,
                run,
                "run.input_received",
                json!({"input_id":i["id"],"kind":"user_message"}),
            )
            .await?;
            m::wake(&mut tx, run, "user_message").await?;
            m::activity(&mut tx, session.id).await?;
            (run, i)
        } else {
            m::start_with_parent(
                &mut tx,
                &session,
                &StartRun {
                    expected_session_revision: a.expected_revision,
                    input,
                },
                None,
            )
            .await?
        };
        let mut result = accepted(&mut tx, session.id, run, i).await?;
        subscribe(&mut tx, s, run, a.on_complete, &mut result).await?;
        attributed(&mut tx, s, op, &a.options.idempotency_key, session.id, run).await?;
        receipts::save(
            &mut tx,
            s.project,
            op,
            s.site,
            &a.options.idempotency_key,
            &hash,
            &receipts::Reply::new(201, result.clone()),
        )
        .await?;
        tx.commit().await?;
        self.notify(run);
        Ok(result)
    }
    async fn site_stop(&self, s: SiteScope, args: Value) -> Result<Value> {
        let a: Stop = decode(args.clone())?;
        key(&a.options.idempotency_key)?;
        if let Some(reason) = &a.reason {
            nonempty(reason, 2048, "Invalid abort reason.")?;
        }
        let op = "site.session.stop";
        let hash = receipts::hash(&args)?;
        let mut tx = self.transaction().await?;
        scope(&mut tx, s).await?;
        if let Some(r) =
            receipts::replay(&mut tx, op, s.site, &a.options.idempotency_key, &hash).await?
        {
            return Ok(r.body);
        }
        let session = session_scope(&mut tx, s, a.session_id).await?;
        harness(&mut tx, s.project, &session.harness_id).await?;
        let run = m::run(&mut tx, a.expected_run_id).await?;
        if run.session_id != session.id {
            return Err(RuntimeError::NotFound);
        }
        run.live()?;
        let i = m::request_abort(&mut tx, &run, a.reason.as_deref(), None).await?;
        let result = accepted(&mut tx, session.id, run.id, i).await?;
        attributed(
            &mut tx,
            s,
            op,
            &a.options.idempotency_key,
            session.id,
            run.id,
        )
        .await?;
        receipts::save(
            &mut tx,
            s.project,
            op,
            s.site,
            &a.options.idempotency_key,
            &hash,
            &receipts::Reply::new(202, result.clone()),
        )
        .await?;
        tx.commit().await?;
        self.notify(run.id);
        Ok(result)
    }
    pub(super) async fn site_metrics(&self, session: Uuid, run: Option<Uuid>) -> Result<Value> {
        super::metrics::read(&mut *self.pool.acquire().await?, session, run).await
    }
}
