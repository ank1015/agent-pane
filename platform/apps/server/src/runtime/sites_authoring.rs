//! Sites authoring authority belongs to a leased Sites harness session, never
//! to site-backend callers. Model arguments cannot choose another site/project.
use super::{
    RuntimeService,
    capabilities::Caller,
    error::{Result, RuntimeError},
    mutations as m,
};
use platform_runtime_contracts::{capabilities::MutationOptions, sites_authoring as a};
use reqwest::Method;
use serde::Deserialize;
use serde_json::{Value, json};
use uuid::Uuid;
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Arguments {
    #[serde(default)]
    input: Option<Value>,
    #[serde(default)]
    options: Option<MutationOptions>,
    #[serde(rename = "operationId")]
    operation_id: Option<Uuid>,
    #[serde(rename = "invocationId")]
    invocation_id: Option<Uuid>,
}
fn input<T: serde::de::DeserializeOwned>(value: Option<Value>) -> Result<T> {
    serde_json::from_value(value.ok_or(RuntimeError::Invalid("Input is required."))?)
        .map_err(|_| RuntimeError::Invalid("Invalid Sites input."))
}
impl RuntimeService {
    pub(super) async fn sites_authoring(
        &self,
        caller: Caller,
        method: &str,
        args: Value,
    ) -> Result<Value> {
        let Caller::Agent { run, .. } = caller else {
            return Err(RuntimeError::Unauthorized);
        };
        if !matches!(
            method,
            "sites.read"
                | "sites.preview"
                | "sites.applyPatch"
                | "sites.operation"
                | "sites.invoke"
                | "sites.invocation"
                | "sites.logs"
                | "sites.query"
                | "sites.execute"
        ) {
            return Err(RuntimeError::Invalid("Unknown Sites authoring method."));
        }
        if args.to_string().len() > 192 * 1024 {
            return Err(RuntimeError::Invalid("Sites arguments exceed limits."));
        }
        let args: Arguments = serde_json::from_value(args.clone())
            .map_err(|_| RuntimeError::Invalid("Invalid Sites arguments."))?;
        let client = self
            .sites
            .as_ref()
            .ok_or(RuntimeError::Invalid("Sites authoring is not configured."))?;
        let mut tx = self.pool.begin().await?;
        super::workers::lock_runs(&mut tx, &[run]).await?;
        let project = caller.authorize(&mut tx).await?;
        let (session,harness,config):(Uuid,String,Value)=sqlx::query_as("select s.id,s.harness_id,s.config from sessions s join runs r on r.session_id=s.id where r.id=$1 for update of s").bind(run).fetch_one(&mut *tx).await?;
        if harness != "sites" {
            return Err(RuntimeError::Unauthorized);
        }
        m::enabled(&mut tx, project, &harness).await?;
        let enabled: bool = sqlx::query_scalar(
            "select exists(select 1 from site_project_access where project_id=$1 and enabled)",
        )
        .bind(project)
        .fetch_one(&mut *tx)
        .await?;
        if !enabled {
            return Err(RuntimeError::Unauthorized);
        }
        let bound: Option<Uuid> = sqlx::query_scalar(
            "select site_id from site_authoring_bindings where session_id=$1 and project_id=$2",
        )
        .bind(session)
        .bind(project)
        .fetch_optional(&mut *tx)
        .await?;
        let site = if let Some(site) = bound {
            site
        } else {
            let site = match config.get("siteId") {
                Some(Value::String(s)) => {
                    Uuid::parse_str(s).map_err(|_| RuntimeError::Configuration)?
                }
                Some(Value::Null) | None => {
                    let id = Uuid::now_v7();
                    sqlx::query(
                        "insert into project_sites(id,project_id,name) values($1,$2,'New site')",
                    )
                    .bind(id)
                    .bind(project)
                    .execute(&mut *tx)
                    .await?;
                    id
                }
                _ => return Err(RuntimeError::Configuration),
            };
            let found:bool=sqlx::query_scalar("select exists(select 1 from project_sites where id=$1 and project_id=$2 and deleted_at is null)").bind(site).bind(project).fetch_one(&mut *tx).await?;
            if !found {
                return Err(RuntimeError::NotFound);
            }
            sqlx::query("insert into site_authoring_bindings values($1,$2,$3,clock_timestamp())")
                .bind(session)
                .bind(site)
                .bind(project)
                .execute(&mut *tx)
                .await?;
            site
        };
        let ready:bool=sqlx::query_scalar("select exists(select 1 from project_sites where id=$1 and project_id=$2 and deleted_at is null and desired_status='ready')").bind(site).bind(project).fetch_one(&mut *tx).await?;
        if !ready {
            return Err(RuntimeError::NotFound);
        }
        let (verb, suffix, mut body) = match method {
            "sites.preview" => (Method::GET, "preview".to_owned(), None),
            "sites.read" => (Method::GET, "source".to_owned(), None),
            "sites.logs" => (Method::GET, "authoring-logs".to_owned(), None),
            "sites.operation" => (
                Method::GET,
                format!(
                    "authoring/{}",
                    args.operation_id
                        .ok_or(RuntimeError::Invalid("operationId is required."))?
                ),
                None,
            ),
            "sites.invocation" => (
                Method::GET,
                format!(
                    "invocations/{}",
                    args.invocation_id
                        .ok_or(RuntimeError::Invalid("invocationId is required."))?
                ),
                None,
            ),
            "sites.query" => {
                let sql: a::Sql = input(args.input.clone())?;
                (Method::POST, "sql/query".into(), Some(json!(sql)))
            }
            "sites.applyPatch" => {
                let patch: a::Patch = input(args.input.clone())?;
                (
                    Method::POST,
                    "authoring".into(),
                    Some(json!({"type":"patch","patch":patch.patch})),
                )
            }
            "sites.execute" => {
                let sql: a::Sql = input(args.input.clone())?;
                (Method::POST, "sql".into(), Some(json!(sql)))
            }
            "sites.invoke" => {
                let request: a::Request = input(args.input.clone())?;
                (
                    Method::POST,
                    "invocations".into(),
                    Some(json!({"request":request,"release_id":null,"timeout_ms":10000})),
                )
            }
            _ => unreachable!(),
        };
        let mutation = matches!(
            method,
            "sites.applyPatch" | "sites.invoke" | "sites.execute"
        );
        let mut operation = None;
        if mutation {
            let key = args
                .options
                .ok_or(RuntimeError::Invalid("Mutation options are required."))?
                .idempotency_key;
            if key.is_empty() || key.len() > 256 || !key.bytes().all(|b| b.is_ascii_graphic()) {
                return Err(RuntimeError::Invalid("Invalid operation key."));
            }
            let request = body.clone().unwrap();
            let proposed = Uuid::now_v7();
            sqlx::query("insert into site_authoring_calls(run_id,operation_key,site_id,method,request,operation_id) values($1,$2,$3,$4,$5,$6) on conflict do nothing").bind(run).bind(&key).bind(site).bind(method).bind(&request).bind(proposed).execute(&mut *tx).await?;
            let (old_method,old_request,id):(String,Value,Uuid)=sqlx::query_as("select method,request,operation_id from site_authoring_calls where run_id=$1 and operation_key=$2").bind(run).bind(key).fetch_one(&mut *tx).await?;
            if old_method != method || old_request != request {
                return Err(RuntimeError::Conflict(
                    "Sites operation key has different input.",
                ));
            }
            operation = Some(id);
            if method != "sites.execute" {
                body.as_mut().unwrap()["id"] = json!(id);
            }
            if method == "sites.invoke" {
                sqlx::query("insert into site_invocations(site_id,id,request) values($1,$2,$3) on conflict do nothing").bind(site).bind(id).bind(body.as_ref().unwrap()).execute(&mut *tx).await?;
            }
        }
        // This commit is admission. Accepted downstream effects can finish after
        // lease loss; all subsequent admissions must authenticate the new owner.
        caller.authorize(&mut tx).await?;
        tx.commit().await?;
        let resource = client
            .call(
                Method::PUT,
                &format!("/internal/sites/{site}"),
                Some(&json!({"project_id":project})),
            )
            .await
            .map_err(upstream)?;
        if resource["status"] != "ready" {
            return Err(RuntimeError::Conflict(
                "Site provisioning is pending or site is suspended; retry the same operation.",
            ));
        }
        if method == "sites.preview" {
            let release = resource["active_release_id"]
                .as_str()
                .and_then(|s| Uuid::parse_str(s).ok())
                .ok_or(RuntimeError::Conflict("No active site code yet."))?;
            return client
                .call(
                    Method::POST,
                    &format!("/internal/sites/{site}/releases/{release}/content-access"),
                    Some(&json!({"ttl_seconds":900})),
                )
                .await
                .map_err(upstream);
        }
        let suffix = if method == "sites.execute" {
            format!("sql/{}", operation.unwrap())
        } else {
            suffix
        };
        let mut result = client
            .call(
                verb,
                &format!("/internal/sites/{site}/{suffix}"),
                body.as_ref(),
            )
            .await
            .map_err(upstream)?;
        if matches!(method, "sites.invoke" | "sites.invocation") {
            if result["response"].to_string().len() > 64 * 1024 {
                result["response"] = Value::Null;
                result["responseTruncated"] = json!(true);
            }
            if let Some(logs) = result["logs"].as_array() {
                let mut saved = vec![];
                let mut size = 0;
                for log in logs {
                    size += log.to_string().len();
                    if size > 32 * 1024 {
                        break;
                    }
                    saved.push(log.clone());
                }
                result["logsTruncated"] = json!(saved.len() < logs.len());
                result["logs"] = json!(saved);
            }
        }
        if result.to_string().len() > 120 * 1024 {
            return Err(RuntimeError::Invalid(
                "Sites result exceeds limits; use bounded queries or individual invocation inspection.",
            ));
        }
        Ok(result)
    }
}
fn upstream(error: crate::sites::Error) -> RuntimeError {
    match error {
        crate::sites::Error::Service(400 | 413 | 422) => RuntimeError::Invalid(
            "Sites rejected the input; check patch context, file limits and backend syntax.",
        ),
        crate::sites::Error::Service(404) => RuntimeError::NotFound,
        crate::sites::Error::Service(409) => {
            RuntimeError::Conflict("Sites state conflicts with this operation.")
        }
        _ => RuntimeError::StoredData,
    }
}
