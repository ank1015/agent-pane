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
#[serde(deny_unknown_fields)]
struct List {
    #[serde(default)]
    options: Options,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
struct EnvironmentId {
    environment_id: Uuid,
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
struct Completion {
    path: String,
    #[serde(default)]
    payload: Value,
}
async fn subscribe(
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
fn key(key: &str) -> Result<()> {
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
async fn scope(tx: &mut Transaction<'_, Postgres>, s: SiteScope) -> Result<()> {
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
async fn harness(
    tx: &mut Transaction<'_, Postgres>,
    project: Uuid,
    id: &str,
) -> Result<(Value, String)> {
    super::project_harnesses::require_enabled(tx, project, id).await?;
    sqlx::query_as("select to_jsonb(h),g.environment_mode from harnesses h join site_harness_grants g on g.harness_id=h.id and g.project_id=$1 where h.id=$2 for share of h,g")
        .bind(project).bind(id).fetch_optional(&mut **tx).await?.ok_or(RuntimeError::Invalid("Harness is not permitted for site invocation."))
}
async fn account(tx: &mut Transaction<'_, Postgres>, project: Uuid, id: Uuid) -> Result<()> {
    let found:Option<Uuid>=sqlx::query_scalar("select account_id from site_account_grants where project_id=$1 and account_id=$2 for share").bind(project).bind(id).fetch_optional(&mut **tx).await?;
    found.ok_or(RuntimeError::Invalid(
        "Account is not permitted for site invocation.",
    ))?;
    Ok(())
}
async fn environment(tx: &mut Transaction<'_, Postgres>, project: Uuid, id: Uuid) -> Result<Value> {
    sqlx::query_scalar("select jsonb_build_object('type',type,'workspace_root',workspace_root,'path',path) || case when type='machine' then jsonb_build_object('machine_id',machine_id) else jsonb_build_object('snapshot_id',snapshot_id) end from project_environments where project_id=$1 and id=$2 for share")
        .bind(project).bind(id).fetch_optional(&mut **tx).await?.ok_or(RuntimeError::NotFound)
}
async fn validate_existing(tx: &mut Transaction<'_, Postgres>, session: &SessionRow) -> Result<()> {
    let (_, mode) = harness(tx, session.project_id, &session.harness_id).await?;
    let id: Uuid = decode(session.config["account_id"].clone())?;
    account(tx, session.project_id, id).await?;
    if mode == "single" {
        let found:bool=sqlx::query_scalar("select exists(select 1 from project_environments where project_id=$1 and (jsonb_build_object('type',type,'workspace_root',workspace_root,'path',path) || case when type='machine' then jsonb_build_object('machine_id',machine_id) else jsonb_build_object('snapshot_id',snapshot_id) end)=$2)")
            .bind(session.project_id).bind(&session.config["environment"]).fetch_one(&mut **tx).await?;
        if !found {
            return Err(RuntimeError::Invalid(
                "Session environment is no longer authorized in this project.",
            ));
        }
    } else if session.config.get("environment").is_some() {
        return Err(RuntimeError::Invalid(
            "Unexpected environment configuration.",
        ));
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
            "environments.list" => {
                let _: std::collections::BTreeMap<String, Value> = decode(args.clone())?;
                if args != json!({}) {
                    return Err(RuntimeError::Invalid("No arguments expected."));
                }
                let items:Vec<Value>=sqlx::query_scalar("select to_jsonb(e) from project_environments e where project_id=$1 order by lower(name),id limit 201").bind(s.project).fetch_all(&self.pool).await?;
                if items.len() > 200 {
                    return Err(RuntimeError::Invalid(
                        "Environment inventory exceeds 200 records.",
                    ));
                }
                Ok(json!({"items":items}))
            }
            "environments.get" => {
                let a: EnvironmentId = decode(args)?;
                sqlx::query_scalar(
                    "select to_jsonb(e) from project_environments e where project_id=$1 and id=$2",
                )
                .bind(s.project)
                .bind(a.environment_id)
                .fetch_optional(&self.pool)
                .await?
                .ok_or(RuntimeError::NotFound)
            }
            "harnesses.list" => {
                if args != json!({}) {
                    return Err(RuntimeError::Invalid("No arguments expected."));
                }
                let items:Vec<Value>=sqlx::query_scalar("select to_jsonb(h) || jsonb_build_object('environmentMode',g.environment_mode) from harnesses h join site_harness_grants g on g.harness_id=h.id and g.project_id=$1 left join project_harnesses p on p.harness_id=h.id and p.project_id=$1 where h.enabled and (h.project_policy='required' or coalesce(p.enabled,false)) order by h.name,h.id")
                    .bind(s.project).fetch_all(&self.pool).await?;
                Ok(json!({"items":items}))
            }
            "harnesses.get" => {
                let a: HarnessId = decode(args)?;
                let mut tx = self.transaction().await?;
                let (mut h, mode) = harness(&mut tx, s.project, &a.harness_id).await?;
                h["environmentMode"] = json!(mode);
                Ok(h)
            }
            "sessions.startOptions" => {
                let a: HarnessId = decode(args)?;
                let mut tx = self.transaction().await?;
                let (h, mode) = harness(&mut tx, s.project, &a.harness_id).await?;
                tx.commit().await?;
                let accounts = self.site_accounts(s.project, &h, providers).await?;
                let props = &h["config_schema"]["properties"];
                Ok(
                    json!({"harnessId":a.harness_id,"environmentRequired":mode=="single","accounts":accounts,"reasoningLevels":props["reasoning_level"]["enum"],"defaultReasoning":h["default_config"]["reasoning_level"],"webSearchSupported":props["web_search_enabled"]["type"]=="boolean","defaultWebSearch":h["default_config"]["web_search_enabled"]}),
                )
            }
            "sessions.start" => self.site_start(s, args, providers).await,
            "sessions.send" => self.site_send(s, args).await,
            "sessions.stop" => self.site_stop(s, args).await,
            "sessions.list" => {
                let a: List = decode(args)?;
                if a.options.after_revision.is_some() || a.options.run_id.is_some() {
                    return Err(RuntimeError::Invalid("Unsupported list options."));
                }
                let mut page = self
                    .sessions(
                        s.project,
                        platform_runtime_contracts::ListQuery {
                            limit: a.options.limit,
                            cursor: a.options.cursor,
                            ..Default::default()
                        },
                    )
                    .await?;
                let ids: Vec<Uuid> = page["items"]
                    .as_array()
                    .ok_or(RuntimeError::StoredData)?
                    .iter()
                    .map(|v| decode(v["id"].clone()))
                    .collect::<Result<_>>()?;
                let latest:Vec<(Uuid,Value)>=sqlx::query_as(&format!("select distinct on (r.session_id) r.session_id,{} from runs r where session_id=any($1) order by session_id,created_at desc,id desc",q::RUN_JSON)).bind(&ids).fetch_all(&self.pool).await?;
                for item in page["items"]
                    .as_array_mut()
                    .ok_or(RuntimeError::StoredData)?
                {
                    let id: Uuid = decode(item["id"].clone())?;
                    item["latest_run"] = latest
                        .iter()
                        .find(|(s, _)| *s == id)
                        .map(|(_, v)| v.clone())
                        .unwrap_or(Value::Null);
                    let current = if item["active_run"].is_null() {
                        &item["latest_run"]
                    } else {
                        &item["active_run"]
                    };
                    item["status"] = current.get("status").cloned().unwrap_or(json!("idle"));
                }
                Ok(page)
            }
            "sessions.get" | "sessions.messages" | "sessions.metrics" => {
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
                match method {
                    "sessions.get" => {
                        if a.options.limit.is_some()
                            || a.options.cursor.is_some()
                            || a.options.after_revision.is_some()
                            || a.options.run_id.is_some()
                        {
                            return Err(RuntimeError::Invalid("Get does not accept options."));
                        }
                        let mut session = self.session(a.session_id).await?;
                        let latest:Option<Value>=sqlx::query_scalar(&format!("select {} from runs r where session_id=$1 order by created_at desc,id desc limit 1",q::RUN_JSON)).bind(a.session_id).fetch_optional(&self.pool).await?;
                        session["latest_run"] = json!(latest);
                        let current = if !session["active_run"].is_null() {
                            &session["active_run"]
                        } else {
                            &session["latest_run"]
                        };
                        session["status"] = current.get("status").cloned().unwrap_or(json!("idle"));
                        Ok(session)
                    }
                    "sessions.messages" => {
                        if a.options.cursor.is_some() {
                            return Err(RuntimeError::Invalid("Messages use afterRevision."));
                        }
                        self.messages(
                            a.session_id,
                            platform_runtime_contracts::MessageQuery {
                                after_revision: a.options.after_revision,
                                limit: a.options.limit,
                                run_id: a.options.run_id,
                            },
                        )
                        .await
                    }
                    _ => {
                        if a.options.limit.is_some()
                            || a.options.cursor.is_some()
                            || a.options.after_revision.is_some()
                        {
                            return Err(RuntimeError::Invalid("Metrics accept runId only."));
                        }
                        self.site_metrics(a.session_id, a.options.run_id).await
                    }
                }
            }
            _ => Err(RuntimeError::Invalid(
                "Unknown or unavailable Platform capability.",
            )),
        }
    }
    async fn site_accounts(
        &self,
        project: Uuid,
        h: &Value,
        providers: &ProviderService,
    ) -> Result<Vec<Value>> {
        let grants: Vec<Uuid> =
            sqlx::query_scalar("select account_id from site_account_grants where project_id=$1")
                .bind(project)
                .fetch_all(&self.pool)
                .await?;
        let accounts = providers.list_accounts().await.map_err(|_| {
            RuntimeError::Conflict("Provider inventory is unavailable. Retry later.")
        })?;
        let mut selected = vec![];
        for account in accounts {
            let a = serde_json::to_value(account).map_err(|_| RuntimeError::StoredData)?;
            let id: Uuid = decode(a["id"].clone())?;
            let provider = a["provider"].as_str().ok_or(RuntimeError::StoredData)?;
            let models = h["supported_models"][provider].as_array();
            if grants.contains(&id)
                && a["status"] == "enabled"
                && models.is_some_and(|m| !m.is_empty())
            {
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
        let (h, _) = harness(&mut tx, s.project, &a.input.harness_id).await?;
        tx.commit().await?;
        let accounts = self.site_accounts(s.project, &h, providers).await?;
        if !accounts.iter().any(|v| {
            v["accountId"] == a.input.account_id.to_string()
                && v["provider"] == a.input.model.provider
                && v["modelIds"]
                    .as_array()
                    .is_some_and(|ids| ids.contains(&json!(a.input.model.id)))
        }) {
            return Err(RuntimeError::Invalid(
                "Select a permitted active account and supported model.",
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
        let (h, mode) = harness(&mut tx, s.project, &a.input.harness_id).await?;
        if !h["supported_models"][&a.input.model.provider]
            .as_array()
            .is_some_and(|ids| ids.contains(&json!(a.input.model.id)))
        {
            return Err(RuntimeError::Configuration);
        }
        account(&mut tx, s.project, a.input.account_id).await?;
        let mut patch = json!({"model":{"provider":a.input.model.provider,"id":a.input.model.id},"account_id":a.input.account_id});
        match (mode.as_str(), a.input.environment_id) {
            ("single", Some(id)) => {
                patch["environment"] = environment(&mut tx, s.project, id).await?
            }
            ("none", None) => {
                patch["environment"] = Value::Null;
            }
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
    async fn site_metrics(&self, session: Uuid, run: Option<Uuid>) -> Result<Value> {
        // Aggregate all canonical assistant messages, not just one UI history page.
        let mut fields = vec![
            "count(*) as assistant_messages".to_string(),
            "count(*) filter(where jsonb_typeof(message->'usage')='object') as messages_with_usage"
                .to_string(),
        ];
        for (name, path) in [
            ("cost_usd", "{usage,cost,total}"),
            ("input_tokens", "{usage,input}"),
            ("output_tokens", "{usage,output}"),
            ("cache_read_tokens", "{usage,cache_read}"),
            ("cache_write_tokens", "{usage,cache_write}"),
        ] {
            // CASE guards the cast even if PostgreSQL reorders predicates.
            let numeric = format!(
                "case when jsonb_typeof(message#>'{path}')='number' then (message#>>'{path}')::numeric end"
            );
            let valid = format!("case when ({numeric})>=0 then ({numeric}) end");
            fields.push(format!("sum({valid}) as {name}"));
            fields.push(format!("count({valid}) as {name}_messages"));
        }
        let sql = format!(
            "select to_jsonb(t) from (select {} from session_messages sm join messages m on m.id=sm.message_id where sm.session_id=$1 and ($2::uuid is null or sm.run_id=$2) and m.message->>'role'='assistant') t",
            fields.join(",")
        );
        let mut metrics: Value = sqlx::query_scalar(&sql)
            .bind(session)
            .bind(run)
            .fetch_one(&self.pool)
            .await?;
        let elapsed:Option<f64>=sqlx::query_scalar("select sum(extract(epoch from (coalesce(finished_at,clock_timestamp())-started_at)))::double precision from runs where session_id=$1 and ($2::uuid is null or id=$2)").bind(session).bind(run).fetch_one(&self.pool).await?;
        metrics["sessionId"] = json!(session);
        metrics["runId"] = json!(run);
        metrics["runWallSeconds"] = json!(elapsed);
        metrics["scope"] = json!(if run.is_some() {
            "run"
        } else {
            "session_history"
        });
        metrics["completeUsage"] = json!(
            metrics["assistant_messages"] != 0
                && [
                    "cost_usd_messages",
                    "input_tokens_messages",
                    "output_tokens_messages",
                    "cache_read_tokens_messages",
                    "cache_write_tokens_messages"
                ]
                .iter()
                .all(|k| metrics[*k] == metrics["assistant_messages"])
        );
        let rate = metrics["input_tokens"]
            .as_f64()
            .zip(metrics["cache_read_tokens"].as_f64())
            .and_then(|(i, c)| if i + c > 0.0 { Some(c / (i + c)) } else { None });
        metrics["cacheHitRate"] = json!(rate);
        Ok(metrics)
    }
}
