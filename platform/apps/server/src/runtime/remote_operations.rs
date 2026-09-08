//! Accepted remote work is durable intent. HTTP handlers never run shell commands
//! or wait for provisioning; a separately leased reconciler drives each resource.
mod runner;
use super::{
    RuntimeService,
    capabilities::Caller,
    error::{Result, RuntimeError},
    mutations as m,
    receipts::{self, Reply},
    workers::lock_runs,
};
use chrono::{DateTime, Utc};
use platform_runtime_contracts::{ExecutionWorkspace, capabilities as c};
use serde::{Deserialize, de::DeserializeOwned};
use serde_json::{Value, json};
use sqlx::{Postgres, Transaction};
use uuid::Uuid;
type Tx<'a> = Transaction<'a, Postgres>;
pub(super) fn is_method(method: &str) -> bool {
    matches!(
        method,
        "sandboxes.createFromSnapshot"
            | "sandboxes.get"
            | "sandboxes.terminate"
            | "execution.bash"
            | "execution.get"
            | "execution.output"
            | "execution.cancel"
    )
}
fn decode<T: DeserializeOwned>(v: Value) -> Result<T> {
    serde_json::from_value(v)
        .map_err(|_| RuntimeError::Invalid("Invalid remote capability arguments."))
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Mutation<T> {
    input: T,
    options: c::MutationOptions,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
struct SandboxId {
    sandbox_id: Uuid,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
struct ExecutionId {
    execution_id: Uuid,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
struct Output {
    execution_id: Uuid,
    #[serde(default)]
    options: c::ExecutionOutputOptions,
}

#[derive(sqlx::FromRow)]
struct Remote {
    id: Uuid,
    project_id: Uuid,
    kind: String,
    environment_id: Option<Uuid>,
    host_id: Option<Uuid>,
    workspace: Value,
    request: Value,
    state: Value,
    status: String,
    error: Option<Value>,
    output: String,
    output_bytes: i64,
    truncated: bool,
    exit_code: Option<i32>,
    cancel_requested: bool,
    cancellation_confirmed: bool,
    created_at: DateTime<Utc>,
    expires_at: DateTime<Utc>,
    finished_at: Option<DateTime<Utc>>,
    process_epoch: i64,
}
fn view(r: &Remote) -> Value {
    if r.kind == "sandbox" {
        json!({"id":r.id,"environmentId":r.environment_id,"status":r.status,"workspace":if r.host_id.is_some() {r.workspace.clone()} else {Value::Null},"error":r.error,"createdAt":r.created_at,"expiresAt":r.expires_at,"terminationRequested":r.cancel_requested,"terminationConfirmed":r.cancellation_confirmed})
    } else {
        json!({"id":r.id,"hostId":r.host_id,"status":r.status,"exitCode":r.exit_code,"error":r.error,"createdAt":r.created_at,"finishedAt":r.finished_at,"cancellationRequested":r.cancel_requested,"cancellationConfirmed":r.cancellation_confirmed,"outputBytes":r.output_bytes,"truncated":r.truncated})
    }
}
async fn scoped(tx: &mut Tx<'_>, project: Uuid, id: Uuid, kind: &str) -> Result<Remote> {
    sqlx::query_as(
        "select * from platform_remote_operations where project_id=$1 and id=$2 and kind=$3",
    )
    .bind(project)
    .bind(id)
    .bind(kind)
    .fetch_optional(&mut **tx)
    .await?
    .ok_or(RuntimeError::NotFound)
}
async fn authorize(tx: &mut Tx<'_>, caller: Caller) -> Result<Uuid> {
    let project = caller.authorize(tx).await?;
    if matches!(caller, Caller::Site(_)) {
        let allowed:bool=sqlx::query_scalar("select execution_enabled from site_project_access where project_id=$1 and enabled for share")
            .bind(project).fetch_optional(&mut **tx).await?.unwrap_or(false);
        if !allowed {
            return Err(RuntimeError::Invalid(
                "Remote execution is not enabled for this site's project.",
            ));
        }
    }
    Ok(project)
}
async fn environment(tx: &mut Tx<'_>, project: Uuid, id: Uuid) -> Result<Value> {
    sqlx::query_scalar(
        "select to_jsonb(e) from project_environments e where project_id=$1 and id=$2 for share",
    )
    .bind(project)
    .bind(id)
    .fetch_optional(&mut **tx)
    .await?
    .ok_or(RuntimeError::NotFound)
}
fn workspace(e: &Value, host: Uuid) -> Result<ExecutionWorkspace> {
    let result = ExecutionWorkspace {
        host_id: host,
        workspace_root: decode(e["workspace_root"].clone())?,
        path: decode(e["path"].clone())?,
        environment_id: Some(decode(e["id"].clone())?),
        sandbox_id: None,
    };
    result.validate().map_err(RuntimeError::Invalid)?;
    Ok(result)
}
fn normalized(path: &str) -> String {
    let windows = path.as_bytes().get(1) == Some(&b':') || path.starts_with("\\\\");
    if windows {
        path.replace('\\', "/").to_lowercase()
    } else {
        path.to_owned()
    }
}
fn within(workdir: &str, w: &ExecutionWorkspace) -> bool {
    let path = normalized(workdir);
    let mut root = normalized(&w.workspace_root);
    if w.path != "." {
        root = format!("{}/{}", root.trim_end_matches('/'), w.path);
    }
    let root = root.trim_end_matches('/');
    !path.split('/').any(|p| p == "..") && (path == root || path.starts_with(&format!("{root}/")))
}
async fn host_workspace(
    tx: &mut Tx<'_>,
    project: Uuid,
    host: Uuid,
    workdir: &str,
) -> Result<ExecutionWorkspace> {
    if !platform_runtime_contracts::is_absolute_workspace_root(workdir) {
        return Err(RuntimeError::Invalid(
            "workdir must be an absolute path in a project workspace.",
        ));
    }
    let rows: Vec<Value>=sqlx::query_scalar("select to_jsonb(e) from project_environments e where project_id=$1 and type='machine' and machine_id=$2 order by length(workspace_root) desc for share")
        .bind(project).bind(host).fetch_all(&mut **tx).await?;
    for e in rows {
        let w = workspace(&e, host)?;
        if within(workdir, &w) {
            return Ok(w);
        }
    }
    let rows:Vec<Value>=sqlx::query_scalar("select workspace from platform_remote_operations where project_id=$1 and kind='sandbox' and host_id=$2 and status='ready' and not cancel_requested and expires_at>clock_timestamp() for share")
        .bind(project).bind(host).fetch_all(&mut **tx).await?;
    for v in rows {
        let w: ExecutionWorkspace = decode(v)?;
        if within(workdir, &w) {
            return Ok(w);
        }
    }
    Err(RuntimeError::NotFound)
}

impl RuntimeService {
    /// Trusted harness bridge for hosts provisioned by the harness itself. A
    /// published output alone never creates this execution association.
    pub(super) async fn bind_harness_workspace(
        &self,
        source: Uuid,
        owner: super::worker_model::Owner,
        key: &str,
        request: c::BindHarnessWorkspace,
    ) -> Result<Reply> {
        let caller = Caller::Agent { run: source, owner };
        let op = "run.workspace.bind";
        let hash = receipts::hash(&request)?;
        let mut tx = self.transaction().await?;
        if let Some(r) = caller.replay(&mut tx, op, key, &hash).await? {
            return Ok(r);
        }
        let project = caller.authorize(&mut tx).await?;
        environment(&mut tx, project, request.environment_id).await?;
        tx.commit().await?;
        let gateway = self.execution.as_ref().ok_or(RuntimeError::Conflict(
            "Execution gateway is not configured.",
        ))?;
        let host = gateway
            .get_host(
                &execution_core::OperationContext::with_timeout(std::time::Duration::from_secs(5)),
                request.host_id,
            )
            .await
            .map_err(|_| {
                RuntimeError::Conflict("Workspace host lookup is unavailable; retry the binding.")
            })?;
        let mut tx = self.transaction().await?;
        if let Some(r) = caller.replay(&mut tx, op, key, &hash).await? {
            return Ok(r);
        }
        lock_runs(&mut tx, &[source]).await?;
        let project = caller.authorize(&mut tx).await?;
        let e = environment(&mut tx, project, request.environment_id).await?;
        let mut w = workspace(&e, request.host_id)?;
        if host.id != request.host_id
            || host.desired_state == execution_api::DesiredHostState::Deleted
        {
            return Err(RuntimeError::NotFound);
        }
        if e["type"] == "machine" {
            if e["machine_id"] != json!(host.id)
                || host.kind != execution_api::ExecutionHostKind::Registered
            {
                return Err(RuntimeError::NotFound);
            }
        } else {
            let binding = host.e2b.as_ref().ok_or(RuntimeError::NotFound)?;
            if host.kind != execution_api::ExecutionHostKind::E2b
                || json!(binding.source)
                    != json!({"type":"snapshot","snapshot_id":e["snapshot_id"]})
            {
                return Err(RuntimeError::NotFound);
            }
            sqlx::query("select pg_advisory_xact_lock(hashtextextended($1,0))")
                .bind(format!("workspace-host:{}", host.id))
                .execute(&mut *tx)
                .await?;
            let existing:Option<(Uuid,Uuid)>=sqlx::query_as("select project_id,id from platform_remote_operations where kind='sandbox' and host_id=$1").bind(host.id).fetch_optional(&mut *tx).await?;
            if let Some((existing, id)) = existing {
                if existing != project {
                    return Err(RuntimeError::NotFound);
                }
                w.sandbox_id = Some(id);
            } else {
                let session = m::run(&mut tx, source).await?.session_id;
                if host.metadata["platform_project_id"] != json!(project)
                    || host.metadata["platform_session_id"] != json!(session)
                {
                    return Err(RuntimeError::Invalid(
                        "Sandbox was not provisioned for this harness session.",
                    ));
                }
                let id = Uuid::now_v7();
                w.sandbox_id = Some(id);
                insert(
                    &mut tx,
                    caller,
                    project,
                    id,
                    "sandbox",
                    Some(request.environment_id),
                    Some(host.id),
                    json!(w),
                    json!({"adopted":true}),
                    "provisioning",
                    binding.timeout_seconds.min(86400) as i64,
                )
                .await?;
                sqlx::query(
                    "update platform_remote_operations set expires_at=$2,state='{}' where id=$1",
                )
                .bind(id)
                .bind(
                    host.created_at
                        + chrono::Duration::seconds(binding.timeout_seconds.min(86400) as i64),
                )
                .execute(&mut *tx)
                .await?;
            }
        }
        let reply = Reply::new(200, json!(w)).owned_by(owner);
        receipts::save(&mut tx, project, op, source, key, &hash, &reply).await?;
        caller.authorize(&mut tx).await?;
        tx.commit().await?;
        Ok(reply)
    }
    pub(super) async fn remote_capability(
        &self,
        caller: Caller,
        method: &str,
        args: Value,
    ) -> Result<Value> {
        if serde_json::to_vec(&args)
            .map_err(|_| RuntimeError::StoredData)?
            .len()
            > 96 * 1024
        {
            return Err(RuntimeError::Invalid("Remote arguments exceed 96 KiB."));
        }
        let read = matches!(
            method,
            "sandboxes.get" | "execution.get" | "execution.output"
        );
        let mut tx = self.transaction().await?;
        if read {
            if let Some(run) = caller.source() {
                lock_runs(&mut tx, &[run]).await?;
            }
            let project = authorize(&mut tx, caller).await?;
            let result = match method {
                "sandboxes.get" => {
                    let a: SandboxId = decode(args)?;
                    view(&scoped(&mut tx, project, a.sandbox_id, "sandbox").await?)
                }
                "execution.get" => {
                    let a: ExecutionId = decode(args)?;
                    view(&scoped(&mut tx, project, a.execution_id, "execution").await?)
                }
                _ => {
                    let a: Output = decode(args)?;
                    output_page(
                        &scoped(&mut tx, project, a.execution_id, "execution").await?,
                        a.options,
                    )?
                }
            };
            caller.authorize(&mut tx).await?;
            tx.commit().await?;
            return Ok(result);
        }
        let mutation: Mutation<Value> = decode(args.clone())?;
        super::site_sdk::key(&mutation.options.idempotency_key)?;
        let key = &mutation.options.idempotency_key;
        let op = caller.operation(&method.to_ascii_lowercase());
        let hash = receipts::hash(&args)?;
        if let Some(r) = caller.replay(&mut tx, &op, key, &hash).await? {
            return Ok(r.body);
        }
        if let Some(run) = caller.source() {
            lock_runs(&mut tx, &[run]).await?;
        }
        let project = authorize(&mut tx, caller).await?;
        let id = match method {
            "sandboxes.createFromSnapshot" => {
                self.execution.as_ref().ok_or(RuntimeError::Conflict(
                    "Execution gateway is not configured.",
                ))?;
                let a: c::CreateSandboxFromSnapshot = decode(mutation.input)?;
                let seconds = a.timeout_seconds.unwrap_or(3600);
                if !(1..=86400).contains(&seconds) {
                    return Err(RuntimeError::Invalid(
                        "Sandbox timeout must be 1–86400 seconds.",
                    ));
                }
                if let Some(name) = &a.name {
                    m_name(name)?;
                }
                let e = environment(&mut tx, project, a.environment_id).await?;
                if e["type"] != "sandbox" {
                    return Err(RuntimeError::Invalid("Choose a snapshot environment."));
                }
                let snapshot: Uuid = decode(e["snapshot_id"].clone())?;
                let id = Uuid::now_v7();
                let request = json!(execution_api::CreateExecutionHostRequest {
                    name: a.name,
                    source: execution_api::E2bHostSource::Snapshot {
                        snapshot_id: snapshot
                    },
                    timeout_seconds: Some(u64::from(seconds)),
                    network_access: Some(true),
                    metadata: json!({"platform_operation_id":id,"platform_project_id":project})
                });
                insert(
                    &mut tx,
                    caller,
                    project,
                    id,
                    "sandbox",
                    Some(a.environment_id),
                    None,
                    json!({"workspace_root":e["workspace_root"],"path":e["path"],"sandbox_id":id}),
                    request,
                    "provisioning",
                    i64::from(seconds),
                )
                .await?;
                id
            }
            "execution.bash" => {
                self.execution.as_ref().ok_or(RuntimeError::Conflict(
                    "Execution gateway is not configured.",
                ))?;
                let a: c::Bash = decode(mutation.input)?;
                let timeout = a
                    .timeout_ms
                    .unwrap_or(tool_bash_minimal::DEFAULT_TIMEOUT_MS);
                if a.command.trim().is_empty()
                    || a.command.len() > 64 * 1024
                    || a.command.contains('\0')
                    || !(1..=tool_bash_minimal::MAX_TIMEOUT_MS).contains(&timeout)
                {
                    return Err(RuntimeError::Invalid(
                        "Invalid command or timeout (maximum 1800000 ms).",
                    ));
                }
                let w = host_workspace(&mut tx, project, a.host_id, &a.workdir).await?;
                let id = Uuid::now_v7();
                insert(
                    &mut tx,
                    caller,
                    project,
                    id,
                    "execution",
                    w.environment_id,
                    Some(a.host_id),
                    json!(w),
                    json!(a),
                    "pending",
                    (timeout / 1000 + 60) as i64,
                )
                .await?;
                id
            }
            "sandboxes.terminate" | "execution.cancel" => {
                let (id, kind) = if method == "sandboxes.terminate" {
                    let a: SandboxId = decode(mutation.input)?;
                    (a.sandbox_id, "sandbox")
                } else {
                    let a: ExecutionId = decode(mutation.input)?;
                    (a.execution_id, "execution")
                };
                let r = scoped(&mut tx, project, id, kind).await?;
                if r.finished_at.is_none() {
                    sqlx::query("update platform_remote_operations set cancel_requested=true,next_attempt_at=clock_timestamp() where id=$1").bind(id).execute(&mut *tx).await?;
                }
                id
            }
            _ => return Err(RuntimeError::Invalid("Unknown remote capability.")),
        };
        let r: Remote = sqlx::query_as("select * from platform_remote_operations where id=$1")
            .bind(id)
            .fetch_one(&mut *tx)
            .await?;
        let result = view(&r);
        if let Some(source) = caller.source() {
            m::event(
                &mut tx,
                source,
                "run.remote_operation",
                json!({"method":method,"operation_id":id}),
            )
            .await?;
        }
        let mut reply = Reply::new(202, result.clone());
        if let Caller::Agent { owner, .. } = caller {
            reply = reply.owned_by(owner);
        }
        receipts::save(&mut tx, project, &op, caller.identity(), key, &hash, &reply).await?;
        caller.authorize(&mut tx).await?;
        tx.commit().await?;
        Ok(result)
    }
}
#[allow(clippy::too_many_arguments)]
async fn insert(
    tx: &mut Tx<'_>,
    caller: Caller,
    project: Uuid,
    id: Uuid,
    kind: &str,
    env: Option<Uuid>,
    host: Option<Uuid>,
    workspace: Value,
    request: Value,
    status: &str,
    seconds: i64,
) -> Result<()> {
    let (site, invocation) = match caller {
        Caller::Site(s) => (Some(s.site), Some(s.invocation)),
        _ => (None, None),
    };
    sqlx::query("insert into platform_remote_operations(id,project_id,kind,environment_id,host_id,workspace,request,status,expires_at,source_run_id,site_id,invocation_id) values($1,$2,$3,$4,$5,$6,$7,$8,clock_timestamp()+make_interval(secs=>$9::double precision),$10,$11,$12)")
        .bind(id).bind(project).bind(kind).bind(env).bind(host).bind(workspace).bind(request).bind(status).bind(seconds as f64).bind(caller.source()).bind(site).bind(invocation).execute(&mut **tx).await?;
    Ok(())
}
fn m_name(name: &str) -> Result<()> {
    super::model::nonempty(
        name,
        128,
        "Sandbox name must contain 1–128 trimmed characters.",
    )
}
fn output_page(r: &Remote, options: c::ExecutionOutputOptions) -> Result<Value> {
    let limit = options.limit_bytes.unwrap_or(32768) as usize;
    if !(1..=65536).contains(&limit) {
        return Err(RuntimeError::Invalid("Output limitBytes must be 1–65536."));
    }
    let offset = if let Some(cursor) = options.cursor {
        let (id, offset) = cursor
            .split_once(':')
            .ok_or(RuntimeError::Invalid("Invalid output cursor."))?;
        if id != r.id.to_string() {
            return Err(RuntimeError::Invalid(
                "Output cursor belongs to another execution.",
            ));
        }
        offset
            .parse::<usize>()
            .map_err(|_| RuntimeError::Invalid("Invalid output cursor."))?
    } else {
        0
    };
    if offset > r.output.len() || !r.output.is_char_boundary(offset) {
        return Err(RuntimeError::Invalid("Invalid output cursor offset."));
    }
    let mut end = (offset + limit).min(r.output.len());
    while !r.output.is_char_boundary(end) {
        end -= 1;
    }
    if end == offset && offset < r.output.len() {
        return Err(RuntimeError::Invalid(
            "limitBytes cannot fit the next UTF-8 character.",
        ));
    }
    let next = if end < r.output.len() || r.finished_at.is_none() {
        Some(format!("{}:{end}", r.id))
    } else {
        None
    };
    Ok(
        json!({"executionId":r.id,"output":&r.output[offset..end],"nextCursor":next,"truncated":r.truncated,"complete":r.finished_at.is_some(),"outputBytes":r.output_bytes}),
    )
}
