//! Shared, authenticated Platform operations for leased agents and site backends.
//! Transport scope is trusted; model arguments never select a project or identity.
use super::{
    RuntimeService, configuration,
    error::{Result, RuntimeError},
    model::{self, SessionRow, StartRun, ValidateStartRun},
    mutations as m, queries as q,
    receipts::{self, Reply},
    site_sdk::{self, SiteScope},
    worker_model::Owner,
    workers::{lock_runs, owned},
};
use crate::providers::ProviderService;
use base64::{Engine, engine::general_purpose::URL_SAFE_NO_PAD};
use platform_runtime_contracts::{HarnessContract, capabilities as c};
use serde::{Deserialize, de::DeserializeOwned};
use serde_json::{Value, json};
use sqlx::{Postgres, Transaction};
use uuid::Uuid;

type Tx<'a> = Transaction<'a, Postgres>;
const PAGE_BYTES: usize = 192 * 1024;

#[derive(Clone, Copy)]
pub(super) enum Caller {
    Site(SiteScope),
    Agent { run: Uuid, owner: Owner },
}
impl Caller {
    pub(super) fn source(self) -> Option<Uuid> {
        match self {
            Self::Agent { run, .. } => Some(run),
            _ => None,
        }
    }
    pub(super) fn identity(self) -> Uuid {
        match self {
            Self::Site(s) => s.site,
            Self::Agent { run, .. } => run,
        }
    }
    pub(super) fn operation(self, method: &str) -> String {
        format!(
            "{}.platform.{method}",
            if matches!(self, Self::Site(_)) {
                "site"
            } else {
                "run"
            }
        )
    }
    pub(super) async fn authorize(self, tx: &mut Tx<'_>) -> Result<Uuid> {
        match self {
            Self::Site(s) => {
                site_sdk::scope(tx, s).await?;
                Ok(s.project)
            }
            Self::Agent { run, owner } => {
                owned(tx, run, owner).await?;
                sqlx::query_scalar("select project_id from runs where id=$1")
                    .bind(run)
                    .fetch_one(&mut **tx)
                    .await
                    .map_err(Into::into)
            }
        }
    }
    pub(super) async fn replay(
        self,
        tx: &mut Tx<'_>,
        op: &str,
        key: &str,
        hash: &str,
    ) -> Result<Option<Reply>> {
        match self {
            Self::Site(s) => {
                site_sdk::scope(tx, s).await?;
                receipts::replay(tx, op, s.site, key, hash).await
            }
            Self::Agent { run, owner } => {
                receipts::worker_replay(tx, op, run, key, hash, owner).await
            }
        }
    }
    async fn harness(self, tx: &mut Tx<'_>, project: Uuid, id: &str) -> Result<Value> {
        match self {
            Self::Site(_) => {
                let (mut h, mode) = site_sdk::harness(tx, project, id).await?;
                let fields: Vec<String> = sqlx::query_scalar("select configurable_fields from site_harness_grants where project_id=$1 and harness_id=$2")
                    .bind(project).bind(id).fetch_one(&mut **tx).await?;
                h["environmentMode"] = json!(mode);
                h["configurableFields"] = json!(fields);
                Ok(h)
            }
            Self::Agent { .. } => {
                m::enabled(tx, project, id).await?;
                sqlx::query_scalar("select to_jsonb(h) from harnesses h where id=$1")
                    .bind(id)
                    .fetch_one(&mut **tx)
                    .await
                    .map_err(Into::into)
            }
        }
    }
    async fn existing(self, tx: &mut Tx<'_>, session: &SessionRow) -> Result<()> {
        if matches!(self, Self::Site(_)) {
            site_sdk::validate_existing(tx, session).await
        } else {
            m::enabled(tx, session.project_id, &session.harness_id).await?;
            validate_frozen_inputs(tx, session).await
        }
    }
}

pub(super) fn is_method(method: &str) -> bool {
    super::remote_operations::is_method(method)
        || matches!(
            method,
            "environments.list"
                | "environments.get"
                | "accounts.list"
                | "harnesses.list"
                | "harnesses.get"
                | "harnesses.startOptions"
                | "sessions.create"
                | "sessions.list"
                | "sessions.get"
                | "sessions.messages"
                | "sessions.stats"
                | "runs.create"
                | "runs.list"
                | "runs.get"
                | "runs.steer"
                | "runs.abort"
                | "runs.stats"
                | "runs.outputs"
        )
}
fn decode<T: DeserializeOwned>(args: Value) -> Result<T> {
    serde_json::from_value(args).map_err(|_| {
        RuntimeError::Invalid("Invalid capability arguments; unknown fields are not supported.")
    })
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct List {
    #[serde(default)]
    options: c::PageOptions,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
struct SessionId {
    session_id: Uuid,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
struct SessionList {
    session_id: Uuid,
    #[serde(default)]
    options: c::PageOptions,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
struct Messages {
    session_id: Uuid,
    #[serde(default)]
    options: c::MessageOptions,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
struct RunId {
    run_id: Uuid,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
struct Outputs {
    run_id: Uuid,
    #[serde(default)]
    options: c::OutputOptions,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
struct HarnessId {
    harness_id: String,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
struct EnvironmentId {
    environment_id: Uuid,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Mutation<T> {
    input: T,
    options: c::MutationOptions,
}

enum Effect {
    Session(c::CreateSession),
    Run(c::CreateRun),
    Steer(c::SteerRun),
    Abort(c::AbortRun),
}
impl Effect {
    fn parse(method: &str, args: Value) -> Result<(Self, String)> {
        Ok(match method {
            "sessions.create" => {
                let a: Mutation<c::CreateSession> = decode(args)?;
                (Self::Session(a.input), a.options.idempotency_key)
            }
            "runs.create" => {
                let a: Mutation<c::CreateRun> = decode(args)?;
                (Self::Run(a.input), a.options.idempotency_key)
            }
            "runs.steer" => {
                let a: Mutation<c::SteerRun> = decode(args)?;
                (Self::Steer(a.input), a.options.idempotency_key)
            }
            "runs.abort" => {
                let a: Mutation<c::AbortRun> = decode(args)?;
                (Self::Abort(a.input), a.options.idempotency_key)
            }
            _ => return Err(RuntimeError::Invalid("Unknown mutation.")),
        })
    }
}

pub(super) async fn validate_frozen_inputs(tx: &mut Tx<'_>, session: &SessionRow) -> Result<()> {
    let value: Value = sqlx::query_scalar("select harness_contract from sessions where id=$1")
        .bind(session.id)
        .fetch_one(&mut **tx)
        .await?;
    let contract: HarnessContract =
        serde_json::from_value(value).map_err(|_| RuntimeError::StoredData)?;
    for id in contract
        .environment_ids(&session.config)
        .map_err(RuntimeError::Invalid)?
    {
        let found: Option<Uuid> = sqlx::query_scalar(
            "select id from project_environments where project_id=$1 and id=$2 for share",
        )
        .bind(session.project_id)
        .bind(id)
        .fetch_optional(&mut **tx)
        .await?;
        found.ok_or(RuntimeError::NotFound)?;
    }
    Ok(())
}
async fn session_scope(tx: &mut Tx<'_>, project: Uuid, id: Uuid) -> Result<()> {
    let found: bool =
        sqlx::query_scalar("select exists(select 1 from sessions where id=$1 and project_id=$2)")
            .bind(id)
            .bind(project)
            .fetch_one(&mut **tx)
            .await?;
    if found {
        Ok(())
    } else {
        Err(RuntimeError::NotFound)
    }
}
async fn run_scope(tx: &mut Tx<'_>, project: Uuid, id: Uuid) -> Result<Uuid> {
    sqlx::query_scalar("select session_id from runs where id=$1 and project_id=$2")
        .bind(id)
        .bind(project)
        .fetch_optional(&mut **tx)
        .await?
        .ok_or(RuntimeError::NotFound)
}
// Lock every involved session, then every involved run in canonical order. This
// matches worker coordination, including two agents operating on each other.
async fn lock_effect(tx: &mut Tx<'_>, caller: Caller, effect: &Effect) -> Result<()> {
    let mut runs: Vec<Uuid> = caller.source().into_iter().collect();
    let mut sessions = vec![];
    match effect {
        Effect::Run(a) => sessions.push(a.session_id),
        Effect::Steer(a) => runs.push(a.run_id),
        Effect::Abort(a) => runs.push(a.run_id),
        _ => (),
    }
    let related: Vec<Uuid> =
        sqlx::query_scalar("select distinct session_id from runs where id=any($1)")
            .bind(&runs)
            .fetch_all(&mut **tx)
            .await?;
    sessions.extend(related);
    sessions.sort();
    sessions.dedup();
    runs.sort();
    runs.dedup();
    for id in sessions {
        m::session(tx, id).await?;
    }
    for id in runs {
        m::run(tx, id).await?;
    }
    Ok(())
}

impl RuntimeService {
    pub(super) async fn platform_capability(
        &self,
        caller: Caller,
        method: &str,
        args: Value,
        providers: Option<&ProviderService>,
    ) -> Result<Value> {
        if method.starts_with("sites.") {
            return self.sites_authoring(caller, method, args).await;
        }
        if super::remote_operations::is_method(method) {
            return self.remote_capability(caller, method, args).await;
        }
        if !is_method(method) {
            return Err(RuntimeError::Invalid(
                "Unknown or unavailable Platform capability.",
            ));
        }
        if serde_json::to_vec(&args)
            .map_err(|_| RuntimeError::StoredData)?
            .len()
            > 192 * 1024
        {
            return Err(RuntimeError::Invalid(
                "Capability arguments exceed 192 KiB.",
            ));
        }
        if matches!(
            method,
            "sessions.create" | "runs.create" | "runs.steer" | "runs.abort"
        ) {
            return self
                .capability_mutation(caller, method, args, providers)
                .await;
        }
        // Provider I/O is outside all SQL locks; authorize again after discovery.
        let needs_accounts = matches!(method, "accounts.list" | "harnesses.startOptions");
        let accounts = if needs_accounts {
            let mut tx = self.transaction().await?;
            caller.authorize(&mut tx).await?;
            tx.commit().await?;
            self.capability_accounts(providers).await?
        } else {
            vec![]
        };
        let mut tx = self.transaction().await?;
        if let Some(source) = caller.source() {
            lock_runs(&mut tx, &[source]).await?;
        }
        let project = caller.authorize(&mut tx).await?;
        let result = self
            .capability_read(&mut tx, caller, project, method, args, accounts)
            .await?;
        caller.authorize(&mut tx).await?;
        if serde_json::to_vec(&result)
            .map_err(|_| RuntimeError::StoredData)?
            .len()
            > 240 * 1024
        {
            return Err(RuntimeError::Invalid(
                "Capability record exceeds the response limit.",
            ));
        }
        tx.commit().await?;
        Ok(result)
    }

    async fn capability_accounts(&self, providers: Option<&ProviderService>) -> Result<Vec<Value>> {
        let providers = providers
            .or(self.providers.as_ref())
            .ok_or(RuntimeError::Conflict(
                "Provider inventory is unavailable. Retry later.",
            ))?;
        let accounts = providers.list_accounts().await.map_err(|_| {
            RuntimeError::Conflict("Provider inventory is unavailable. Retry later.")
        })?;
        accounts.into_iter().map(|account| {
            let a = serde_json::to_value(account).map_err(|_| RuntimeError::StoredData)?;
            // Explicit projection excludes credential metadata and provider config.
            Ok(json!({"accountId":a["id"],"name":a["name"],"provider":a["provider"],"status":a["status"]}))
        }).collect()
    }

    async fn capability_read(
        &self,
        tx: &mut Tx<'_>,
        caller: Caller,
        project: Uuid,
        method: &str,
        args: Value,
        mut accounts: Vec<Value>,
    ) -> Result<Value> {
        match method {
            "accounts.list" | "harnesses.startOptions" => {
                if matches!(caller, Caller::Site(_)) {
                    let grants: Vec<Uuid> = sqlx::query_scalar(
                        "select account_id from site_account_grants where project_id=$1",
                    )
                    .bind(project)
                    .fetch_all(&mut **tx)
                    .await?;
                    accounts.retain(|a| grants.iter().any(|id| a["accountId"] == id.to_string()));
                }
                if method == "accounts.list" {
                    let a: List = decode(args)?;
                    return inventory_page(
                        accounts,
                        a.options,
                        &format!("accounts:{project}:{}", caller.identity()),
                        "accountId",
                    );
                }
                let a: HarnessId = decode(args)?;
                let h = caller.harness(tx, project, &a.harness_id).await?;
                let selected: Vec<Value> = accounts.iter().filter_map(|a| {
                    let models = &h["supported_models"][a["provider"].as_str()?];
                    (a["status"]=="enabled" && models.as_array().is_some_and(|m| !m.is_empty())).then(||json!({"accountId":a["accountId"],"name":a["name"],"provider":a["provider"],"modelIds":models}))
                }).collect();
                Ok(
                    json!({"harnessId":a.harness_id,"accounts":selected,"configSchema":h["config_schema"],"defaultConfig":h["default_config"],"harnessContract":h["harness_contract"],"configurableFields":h["configurableFields"],"environmentMode":h["environmentMode"]}),
                )
            }
            "environments.list" => {
                let a: List = decode(args)?;
                let tag = format!("environments:{project}");
                let after = inventory_cursor(a.options.cursor.as_deref(), &tag)?;
                let count = model::limit(a.options.limit)?;
                let rows: Vec<Value> = sqlx::query_scalar("select to_jsonb(e) from project_environments e where project_id=$1 and id::text>$2 order by id limit $3")
                    .bind(project).bind(after).bind(count+1).fetch_all(&mut **tx).await?;
                inventory_page(rows, a.options, &tag, "id")
            }
            "environments.get" => {
                let a: EnvironmentId = decode(args)?;
                sqlx::query_scalar(
                    "select to_jsonb(e) from project_environments e where project_id=$1 and id=$2",
                )
                .bind(project)
                .bind(a.environment_id)
                .fetch_optional(&mut **tx)
                .await?
                .ok_or(RuntimeError::NotFound)
            }
            "harnesses.list" => {
                let a: List = decode(args)?;
                let tag = format!("harnesses:{project}:{}", caller.identity());
                let after = inventory_cursor(a.options.cursor.as_deref(), &tag)?;
                let count = model::limit(a.options.limit)?;
                let rows: Vec<Value> = sqlx::query_scalar(r#"select to_jsonb(h) || case when $2 then jsonb_build_object('environmentMode',g.environment_mode,'configurableFields',g.configurable_fields) else '{}'::jsonb end from harnesses h left join project_harnesses p on p.harness_id=h.id and p.project_id=$1 left join site_harness_grants g on g.harness_id=h.id and g.project_id=$1 where h.enabled and (h.project_policy='required' or coalesce(p.enabled,false)) and (not $2 or g.harness_id is not null) and h.id collate "C">$3 order by h.id collate "C" limit $4"#)
                    .bind(project).bind(matches!(caller,Caller::Site(_))).bind(after).bind(count+1).fetch_all(&mut **tx).await?;
                inventory_page(rows, a.options, &tag, "id")
            }
            "harnesses.get" => {
                let a: HarnessId = decode(args)?;
                caller.harness(tx, project, &a.harness_id).await
            }
            "sessions.list" => {
                let a: List = decode(args)?;
                let mut page = Self::sessions_on(
                    tx,
                    project,
                    platform_runtime_contracts::ListQuery {
                        limit: a.options.limit,
                        cursor: a.options.cursor,
                        ..Default::default()
                    },
                )
                .await?;
                let items = page["items"]
                    .as_array_mut()
                    .ok_or(RuntimeError::StoredData)?;
                enrich_sessions(tx, items).await?;
                bound_cursor_page(
                    page,
                    &format!("sessions:v1:{project}:false"),
                    "last_activity_at",
                )
            }
            "sessions.get" => {
                let a: SessionId = decode(args)?;
                session_scope(tx, project, a.session_id).await?;
                let mut record = q::session_json(tx, a.session_id).await?;
                enrich_sessions(tx, std::slice::from_mut(&mut record)).await?;
                Ok(record)
            }
            "sessions.messages" => {
                let a: Messages = decode(args)?;
                session_scope(tx, project, a.session_id).await?;
                let page = Self::messages_on(
                    tx,
                    a.session_id,
                    platform_runtime_contracts::MessageQuery {
                        limit: a.options.limit,
                        after_revision: a.options.after_revision,
                        run_id: a.options.run_id,
                    },
                )
                .await?;
                bound_sequence_page(page, "revision", "next_after_revision")
            }
            "sessions.stats" => {
                let a: SessionId = decode(args)?;
                session_scope(tx, project, a.session_id).await?;
                Self::capability_stats(tx, a.session_id, None).await
            }
            "runs.list" => {
                let a: SessionList = decode(args)?;
                session_scope(tx, project, a.session_id).await?;
                let page = Self::runs_on(
                    tx,
                    a.session_id,
                    false,
                    platform_runtime_contracts::ListQuery {
                        limit: a.options.limit,
                        cursor: a.options.cursor,
                        ..Default::default()
                    },
                )
                .await?;
                bound_cursor_page(
                    page,
                    &format!("runs:v1:{}:false:None", a.session_id),
                    "created_at",
                )
            }
            "runs.get" | "runs.stats" => {
                let a: RunId = decode(args)?;
                let session = run_scope(tx, project, a.run_id).await?;
                if method == "runs.get" {
                    q::run_json(tx, a.run_id).await
                } else {
                    Self::capability_stats(tx, session, Some(a.run_id)).await
                }
            }
            "runs.outputs" => {
                let a: Outputs = decode(args)?;
                Self::read_run_outputs(tx, project, a.run_id, a.options.into()).await
            }
            _ => Err(RuntimeError::Invalid("Unknown capability.")),
        }
    }

    async fn capability_stats(tx: &mut Tx<'_>, session: Uuid, run: Option<Uuid>) -> Result<Value> {
        let old = super::metrics::read(tx, session, run).await?;
        let mut result = json!({"sessionId":session,"runId":run,"assistantMessages":old["assistant_messages"],"runWallSeconds":old["runWallSeconds"]});
        for (camel, snake) in [
            ("inputTokens", "input_tokens"),
            ("outputTokens", "output_tokens"),
            ("cacheReadTokens", "cache_read_tokens"),
            ("cacheWriteTokens", "cache_write_tokens"),
            ("costUsd", "cost_usd"),
        ] {
            result[camel] =
                json!({"total":old[snake],"contributingMessages":old[format!("{snake}_messages")]});
        }
        Ok(result)
    }

    async fn capability_mutation(
        &self,
        caller: Caller,
        method: &str,
        args: Value,
        providers: Option<&ProviderService>,
    ) -> Result<Value> {
        let hash = receipts::hash(&args)?;
        let (effect, key) = Effect::parse(method, args)?;
        site_sdk::key(&key)?;
        let op = caller.operation(method);
        // Resolve receipts before mutable discovery, but never authorize a new effect
        // from a stale source lease. Provider calls hold no database locks.
        let mut tx = self.transaction().await?;
        if let Some(reply) = caller.replay(&mut tx, &op, &key, &hash).await? {
            return Ok(reply.body);
        }
        caller.authorize(&mut tx).await?;
        tx.commit().await?;
        let accounts = if matches!(effect, Effect::Session(_) | Effect::Run(_)) {
            self.capability_accounts(providers).await?
        } else {
            vec![]
        };
        let mut tx = self.transaction().await?;
        if let Some(reply) = caller.replay(&mut tx, &op, &key, &hash).await? {
            return Ok(reply.body);
        }
        lock_effect(&mut tx, caller, &effect).await?;
        let project = caller.authorize(&mut tx).await?;
        let (session, run, input, callback, status) = match effect {
            Effect::Session(mut a) => {
                model::validate_title(a.title.as_deref())?;
                if a.on_complete.is_some() && a.initial_input.is_none() {
                    return Err(RuntimeError::Invalid(
                        "onComplete requires an initial input.",
                    ));
                }
                if let Some(input) = &a.initial_input {
                    model::validate_user_message(input)?;
                }
                let h = caller.harness(&mut tx, project, &a.harness_id).await?;
                if let Some(id) = a.config.get("account_id")
                    && *id != json!(a.account_id)
                {
                    return Err(RuntimeError::Invalid(
                        "accountId conflicts with config.account_id.",
                    ));
                }
                if matches!(caller, Caller::Site(_)) {
                    let allowed = h["configurableFields"]
                        .as_array()
                        .ok_or(RuntimeError::StoredData)?;
                    if a.config
                        .keys()
                        .any(|k| k != "account_id" && !allowed.contains(&json!(k)))
                    {
                        return Err(RuntimeError::Invalid(
                            "Configuration field is not permitted by this site's harness grant.",
                        ));
                    }
                    site_sdk::account(&mut tx, project, a.account_id).await?;
                }
                a.config.insert("account_id".into(), json!(a.account_id));
                let config = configuration::resolve(&mut tx, &a.harness_id, &a.config).await?;
                validate_account_model(&config, &h, &accounts)?;
                super::run_outputs::validate_inputs(&mut tx, project, &a.harness_id, &config)
                    .await?;
                let session = Uuid::now_v7();
                sqlx::query("insert into sessions(id,project_id,harness_id,title,config) values($1,$2,$3,$4,$5)")
                    .bind(session).bind(project).bind(a.harness_id).bind(a.title).bind(config).execute(&mut *tx).await?;
                let row = m::session(&mut tx, session).await?;
                // Legacy grants still constrain their environment object. Declared
                // grants accept any number of references from the frozen contract.
                caller.existing(&mut tx, &row).await?;
                let (run, input) = if let Some(input) = a.initial_input {
                    let (id, i) = m::start_with_parent(
                        &mut tx,
                        &row,
                        &StartRun {
                            expected_session_revision: 0,
                            input,
                        },
                        caller.source(),
                    )
                    .await?;
                    (Some(id), i)
                } else {
                    (None, Value::Null)
                };
                (session, run, input, a.on_complete, 201)
            }
            Effect::Run(a) => {
                session_scope(&mut tx, project, a.session_id).await?;
                let row = m::session(&mut tx, a.session_id).await?;
                caller.existing(&mut tx, &row).await?;
                let h = caller.harness(&mut tx, project, &row.harness_id).await?;
                validate_account_model(&row.config, &h, &accounts)?;
                let request = StartRun {
                    expected_session_revision: a.expected_revision,
                    input: a.input,
                };
                request.validate()?;
                let (run, input) =
                    m::start_with_parent(&mut tx, &row, &request, caller.source()).await?;
                (row.id, Some(run), input, a.on_complete, 201)
            }
            Effect::Steer(a) => {
                model::validate_user_message(&a.input)?;
                let session = run_scope(&mut tx, project, a.run_id).await?;
                let row = m::session(&mut tx, session).await?;
                caller.existing(&mut tx, &row).await?;
                let run = m::run(&mut tx, a.run_id).await?;
                run.live()?;
                let payload = json!({"message":a.input});
                let dedup = receipts::hash(&(caller.identity(), &op, &key))?;
                let input = m::input_from(
                    &mut tx,
                    project,
                    a.run_id,
                    "user_message",
                    payload.clone(),
                    &dedup,
                    caller.source(),
                )
                .await?;
                m::satisfy_inputs(&mut tx, a.run_id, "user_message", &payload, &input["id"])
                    .await?;
                super::waits::resolve(&mut tx, a.run_id).await?;
                m::event(&mut tx,a.run_id,"run.input_received",json!({"input_id":input["id"],"kind":"user_message","source_run_id":caller.source()})).await?;
                m::wake(&mut tx, a.run_id, "user_message").await?;
                m::activity(&mut tx, session).await?;
                (session, Some(a.run_id), input, None, 202)
            }
            Effect::Abort(a) => {
                if let Some(reason) = &a.reason {
                    model::nonempty(reason, 2048, "Invalid abort reason.")?;
                }
                let session = run_scope(&mut tx, project, a.run_id).await?;
                let row = m::session(&mut tx, session).await?;
                // Aborting does not depend on the account or environment still existing.
                if matches!(caller, Caller::Site(_)) {
                    caller.harness(&mut tx, project, &row.harness_id).await?;
                }
                let run = m::run(&mut tx, a.run_id).await?;
                run.live()?;
                let input =
                    m::request_abort(&mut tx, &run, a.reason.as_deref(), caller.source()).await?;
                (session, Some(a.run_id), input, None, 202)
            }
        };
        let mut result = json!({"session":q::session_json(&mut tx,session).await?,"run":match run {Some(id)=>q::run_json(&mut tx,id).await?,None=>Value::Null},"input":input,"callback":null});
        if let Some(callback) = callback {
            let Caller::Site(s) = caller else {
                return Err(RuntimeError::Invalid(
                    "onComplete requires a site backend invocation.",
                ));
            };
            site_sdk::subscribe(
                &mut tx,
                s,
                run.ok_or(RuntimeError::Invalid("Callback requires a run."))?,
                Some(site_sdk::Completion {
                    path: callback.path,
                    payload: callback.payload,
                }),
                &mut result,
            )
            .await?;
        }
        match caller {
            Caller::Site(s) => {
                sqlx::query("insert into site_runtime_operations(site_id,invocation_id,operation,operation_key,session_id,run_id) values($1,$2,$3,$4,$5,$6)")
                    .bind(s.site).bind(s.invocation).bind(&op).bind(&key).bind(session).bind(run).execute(&mut *tx).await?;
            }
            Caller::Agent { run: source, .. } => {
                m::event(
                    &mut tx,
                    source,
                    "run.platform_operation",
                    json!({"method":method,"session_id":session,"run_id":run}),
                )
                .await?;
                m::hint(&mut tx, source).await?;
            }
        }
        let mut reply = Reply::new(status, result.clone());
        if let Caller::Agent { owner, .. } = caller {
            reply = reply.owned_by(owner);
        }
        if serde_json::to_vec(&result)
            .map_err(|_| RuntimeError::StoredData)?
            .len()
            > 240 * 1024
        {
            return Err(RuntimeError::Invalid(
                "Capability result exceeds the response limit.",
            ));
        }
        receipts::save(
            &mut tx,
            project,
            &op,
            caller.identity(),
            &key,
            &hash,
            &reply,
        )
        .await?;
        caller.authorize(&mut tx).await?;
        tx.commit().await?;
        if let Some(run) = run {
            self.notify(run);
        }
        if let Some(source) = caller.source() {
            self.notify(source);
        }
        Ok(result)
    }
}

fn validate_account_model(config: &Value, harness: &Value, accounts: &[Value]) -> Result<()> {
    let provider = config["model"]["provider"]
        .as_str()
        .ok_or(RuntimeError::Configuration)?;
    if !harness["supported_models"][provider]
        .as_array()
        .is_some_and(|ids| ids.contains(&config["model"]["id"]))
        || !accounts.iter().any(|a| {
            a["accountId"] == config["account_id"]
                && a["provider"] == provider
                && a["status"] == "enabled"
        })
    {
        return Err(RuntimeError::Invalid(
            "Select a permitted active account and supported model.",
        ));
    }
    Ok(())
}

#[derive(serde::Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct InventoryCursor {
    tag: String,
    after: String,
}
fn inventory_cursor(raw: Option<&str>, tag: &str) -> Result<String> {
    let Some(raw) = raw else {
        return Ok(String::new());
    };
    let invalid = || RuntimeError::Invalid("Invalid cursor for this collection.");
    if raw.len() > 2048 {
        return Err(invalid());
    }
    let c: InventoryCursor =
        serde_json::from_slice(&URL_SAFE_NO_PAD.decode(raw).map_err(|_| invalid())?)
            .map_err(|_| invalid())?;
    if c.tag != tag {
        return Err(invalid());
    }
    Ok(c.after)
}
fn page_length(items: &[Value], limit: usize) -> Result<usize> {
    let mut bytes = 0;
    let mut count = 0;
    for item in items.iter().take(limit) {
        let size = serde_json::to_vec(item)
            .map_err(|_| RuntimeError::StoredData)?
            .len();
        if bytes + size > PAGE_BYTES {
            break;
        }
        count += 1;
        bytes += size;
    }
    if !items.is_empty() && count == 0 {
        return Err(RuntimeError::Invalid(
            "An individual record exceeds the capability page limit.",
        ));
    }
    Ok(count)
}
fn inventory_page(
    mut items: Vec<Value>,
    options: c::PageOptions,
    tag: &str,
    id: &str,
) -> Result<Value> {
    let limit = model::limit(options.limit)? as usize;
    let after = inventory_cursor(options.cursor.as_deref(), tag)?;
    items.sort_by(|a, b| a[id].as_str().cmp(&b[id].as_str()));
    items.retain(|v| v[id].as_str().is_some_and(|v| v > after.as_str()));
    let count = page_length(&items, limit)?;
    let more = items.len() > count;
    items.truncate(count);
    let next = if more {
        Some(
            URL_SAFE_NO_PAD.encode(
                serde_json::to_vec(&InventoryCursor {
                    tag: tag.into(),
                    after: items.last().unwrap()[id]
                        .as_str()
                        .ok_or(RuntimeError::StoredData)?
                        .into(),
                })
                .map_err(|_| RuntimeError::StoredData)?,
            ),
        )
    } else {
        None
    };
    Ok(json!({"items":items,"next_cursor":next}))
}
fn bound_cursor_page(page: Value, tag: &str, column: &str) -> Result<Value> {
    let items = page["items"].as_array().ok_or(RuntimeError::StoredData)?;
    let count = page_length(items, items.len())?;
    if count == items.len() {
        return Ok(page);
    }
    q::page(items.clone(), count as i64, tag.into(), column)
}
fn bound_sequence_page(mut page: Value, field: &str, next: &str) -> Result<Value> {
    let items = page["items"].as_array().ok_or(RuntimeError::StoredData)?;
    let count = page_length(items, items.len())?;
    if count < items.len() {
        page = q::sequence_page(items.clone(), count as i64, field, next);
    }
    Ok(page)
}

async fn enrich_sessions(tx: &mut Tx<'_>, items: &mut [Value]) -> Result<()> {
    let ids: Vec<Uuid> = items
        .iter()
        .map(|v| decode(v["id"].clone()))
        .collect::<Result<_>>()?;
    let latest: Vec<(Uuid,Value)>=sqlx::query_as(&format!("select distinct on (r.session_id) r.session_id,{} from runs r where session_id=any($1) order by session_id,created_at desc,id desc",q::RUN_JSON)).bind(ids).fetch_all(&mut **tx).await?;
    for item in items {
        let id: Uuid = decode(item["id"].clone())?;
        item["latest_run"] = latest
            .iter()
            .find(|(session, _)| *session == id)
            .map(|(_, v)| v.clone())
            .unwrap_or(Value::Null);
        let current = if item["active_run"].is_null() {
            &item["latest_run"]
        } else {
            &item["active_run"]
        };
        item["status"] = current.get("status").cloned().unwrap_or(json!("idle"));
    }
    Ok(())
}
