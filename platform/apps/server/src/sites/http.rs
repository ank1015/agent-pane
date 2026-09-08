use super::*;
use axum::{
    Router,
    extract::{DefaultBodyLimit, Path, Query, Request, State},
    http::{HeaderMap, HeaderValue},
    middleware::{self, Next},
    routing::{get, post, put},
};
use serde::{Deserialize, Serialize};
use serde_json::Value;

pub fn router(service: SitesService) -> Router {
    Router::new()
        .route("/api/projects/{project}/sites", get(list))
        .route(
            "/api/projects/{project}/sites/{site}",
            put(create).get(detail).patch(update).delete(remove),
        )
        .route(
            "/api/projects/{project}/sites/{site}/content-access",
            post(content),
        )
        .route(
            "/api/projects/{project}/sites/{site}/invocations",
            post(invoke),
        )
        .route(
            "/api/projects/{project}/sites/{site}/invocations/{invocation}",
            get(inspect),
        )
        .route(
            "/api/projects/{project}/sites/{site}/diagnostics",
            get(diagnostics),
        )
        .route(
            "/api/projects/{project}/sites/{site}/sessions",
            get(authoring_sessions),
        )
        .route(
            "/api/projects/{project}/sites/{site}/source",
            get(source).patch(edit),
        )
        .route(
            "/api/projects/{project}/sites/{site}/snapshots",
            get(snapshots).post(snapshot),
        )
        .route(
            "/api/projects/{project}/sites/{site}/snapshots/{snapshot}/restore",
            post(restore),
        )
        .route(
            "/api/projects/{project}/sites/{site}/authoring/{operation}",
            get(authoring_operation),
        )
        .route("/internal/site-access/{project}", put(access))
        .route(
            "/api/projects/{project}/sites/{site}/callbacks",
            get(callback_list),
        )
        .route(
            "/api/projects/{project}/sites/{site}/callbacks/{callback}",
            get(callback_get),
        )
        .route(
            "/api/projects/{project}/sites/{site}/callbacks/{callback}/retry",
            post(callback_retry),
        )
        .route("/internal/site-capabilities", post(capability))
        .layer(DefaultBodyLimit::max(256 * 1024))
        .layer(middleware::from_fn_with_state(
            service.clone(),
            authenticate,
        ))
        .with_state(service)
}
fn bearer(headers: &HeaderMap) -> Result<&str> {
    let mut values = headers.get_all("authorization").iter();
    let value = values
        .next()
        .and_then(|v| v.to_str().ok())
        .and_then(|s| s.strip_prefix("Bearer "))
        .ok_or(Error::Unauthorized)?;
    if values.next().is_some() || validate_token(value).is_err() {
        return Err(Error::Unauthorized);
    }
    Ok(value)
}
async fn authenticate(
    State(service): State<SitesService>,
    request: Request,
    next: Next,
) -> Response {
    let credentials = bearer(request.headers()).map(token_hash);
    let path = request.uri().path().to_owned();
    let permitted=async {
        let hash=credentials?;
        if path=="/internal/site-capabilities" {
            if hash!=service.capability_hash{return Err(Error::Unauthorized);}
        } else if path.starts_with("/internal/site-access/") {
            if service.admin_hash.as_deref()!=Some(&hash){return Err(Error::Unauthorized);}
        } else {
            let project=path.split('/').nth(3).and_then(|s|Uuid::parse_str(s).ok()).ok_or(Error::Unauthorized)?;
            let allowed:bool=sqlx::query_scalar("select exists(select 1 from site_project_access where project_id=$1 and token_hash=$2 and enabled)")
                .bind(project).bind(hash).fetch_one(&service.pool).await?;
            if !allowed{return Err(Error::Unauthorized);}
        }
        Ok::<_,Error>(())
    }.await;
    let mut response = match permitted {
        Ok(()) => next.run(request).await,
        Err(e) => e.into_response(),
    };
    response
        .headers_mut()
        .insert("cache-control", HeaderValue::from_static("no-store"));
    response.headers_mut().insert(
        "x-request-id",
        HeaderValue::from_str(&Uuid::now_v7().to_string()).unwrap(),
    );
    response
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
struct Grant {
    id: String,
    #[serde(default = "declared_mode")]
    environment_mode: String,
    #[serde(default)]
    configurable_fields: Vec<String>,
}
fn declared_mode() -> String {
    "declared".into()
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
struct Access {
    token: String,
    enabled: bool,
    #[serde(default)]
    execution_enabled: bool,
    harnesses: Vec<Grant>,
    account_ids: Vec<Uuid>,
}
async fn access(
    State(s): State<SitesService>,
    Path(project): Path<Uuid>,
    Json(input): Json<Access>,
) -> Result<Json<Value>> {
    validate_token(&input.token)?;
    if input.harnesses.len() > 64 || input.account_ids.len() > 64 {
        return Err(invalid("At most 64 harness and account grants."));
    }
    let hash = token_hash(&input.token);
    if hash == s.capability_hash || s.admin_hash.as_deref() == Some(&hash) {
        return Err(invalid(
            "Project credentials must differ from service/admin credentials.",
        ));
    }
    let mut tx = s.pool.begin().await?;
    sqlx::query_scalar::<_, Uuid>("select project_id from projects where project_id=$1 for update")
        .bind(project)
        .fetch_optional(&mut *tx)
        .await?
        .ok_or(Error::NotFound)?;
    sqlx::query("insert into site_project_access(project_id,token_hash,enabled,execution_enabled) values($1,$2,$3,$4) on conflict(project_id) do update set token_hash=$2,enabled=$3,execution_enabled=$4")
        .bind(project).bind(hash).bind(input.enabled).bind(input.execution_enabled).execute(&mut *tx).await?;
    sqlx::query("delete from site_harness_grants where project_id=$1")
        .bind(project)
        .execute(&mut *tx)
        .await?;
    sqlx::query("delete from site_account_grants where project_id=$1")
        .bind(project)
        .execute(&mut *tx)
        .await?;
    for grant in input.harnesses {
        if !platform_runtime_contracts::is_valid_harness_id(&grant.id)
            || !matches!(
                grant.environment_mode.as_str(),
                "none" | "single" | "declared"
            )
            || grant.configurable_fields.len() > 64
            || grant.configurable_fields.iter().any(|f| {
                f.is_empty()
                    || f.len() > 128
                    || f.chars().any(char::is_control)
                    || f == "account_id"
            })
        {
            return Err(invalid("Invalid harness grant."));
        }
        sqlx::query("insert into site_harness_grants(project_id,harness_id,environment_mode,configurable_fields) values($1,$2,$3,$4)").bind(project).bind(grant.id).bind(grant.environment_mode).bind(grant.configurable_fields).execute(&mut *tx).await?;
    }
    for id in input.account_ids {
        sqlx::query("insert into site_account_grants(project_id,account_id) values($1,$2) on conflict do nothing").bind(project).bind(id).execute(&mut *tx).await?;
    }
    tx.commit().await?;
    Ok(Json(json!({"projectId":project,"enabled":input.enabled})))
}
async fn list(State(s): State<SitesService>, Path(project): Path<Uuid>) -> Result<Json<Value>> {
    let items:Vec<Value>=sqlx::query_scalar("select to_jsonb(s) from project_sites s where project_id=$1 and deleted_at is null order by id limit 101").bind(project).fetch_all(&s.pool).await?;
    // Small v1 collection; never silently hide an oversized project.
    if items.len() > 100 {
        return Err(invalid("Site list exceeds the v1 limit of 100."));
    }
    Ok(Json(json!({"items":items})))
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Create {
    name: String,
}
fn name(value: &str) -> Result<()> {
    if value.is_empty()
        || value.trim() != value
        || value.chars().count() > 128
        || value.chars().any(char::is_control)
    {
        Err(invalid("Name must contain 1–128 trimmed characters."))
    } else {
        Ok(())
    }
}
async fn create(
    State(s): State<SitesService>,
    Path((project, site)): Path<(Uuid, Uuid)>,
    Json(input): Json<Create>,
) -> Result<Json<Value>> {
    name(&input.name)?;
    sqlx::query(
        "insert into project_sites(id,project_id,name) values($1,$2,$3) on conflict(id) do nothing",
    )
    .bind(site)
    .bind(project)
    .bind(&input.name)
    .execute(&s.pool)
    .await?;
    let record = s.site(project, site).await?;
    if record["name"] != input.name {
        return Err(Error::Conflict);
    }
    let resource = s.reconcile(project, site).await?;
    Ok(Json(json!({"site":record,"resource":resource})))
}
async fn detail(
    State(s): State<SitesService>,
    Path((project, site)): Path<(Uuid, Uuid)>,
) -> Result<Json<Value>> {
    let record = s.site(project, site).await?;
    let resource = s.reconcile(project, site).await?;
    Ok(Json(json!({"site":record,"resource":resource})))
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Update {
    name: Option<String>,
    status: Option<String>,
}
async fn update(
    State(s): State<SitesService>,
    Path((project, site)): Path<(Uuid, Uuid)>,
    Json(input): Json<Update>,
) -> Result<Json<Value>> {
    if input.name.is_none() && input.status.is_none() {
        return Err(invalid("Provide name or status."));
    }
    if let Some(n) = &input.name {
        name(n)?;
    }
    if input
        .status
        .as_deref()
        .is_some_and(|v| !matches!(v, "ready" | "suspended"))
    {
        return Err(invalid("Invalid site status."));
    }
    s.site(project, site).await?;
    sqlx::query("update project_sites set name=coalesce($3,name),desired_status=coalesce($4,desired_status),updated_at=clock_timestamp() where project_id=$1 and id=$2 and deleted_at is null")
        .bind(project).bind(site).bind(input.name).bind(input.status).execute(&s.pool).await?;
    Ok(Json(
        json!({"site":s.site(project,site).await?,"resource":s.reconcile(project,site).await?}),
    ))
}
async fn remove(
    State(s): State<SitesService>,
    Path((project, site)): Path<(Uuid, Uuid)>,
) -> Result<StatusCode> {
    let found=sqlx::query("update project_sites set deleted_at=coalesce(deleted_at,clock_timestamp()),desired_status='suspended',updated_at=clock_timestamp() where project_id=$1 and id=$2")
        .bind(project).bind(site).execute(&s.pool).await?;
    if found.rows_affected() == 0 {
        return Err(Error::NotFound);
    }
    // Logical deletion is durable even if physical suspension needs reconciliation.
    let _ = s
        .client
        .call(
            reqwest::Method::PATCH,
            &format!("/internal/sites/{site}"),
            Some(&json!({"status":"suspended"})),
        )
        .await;
    Ok(StatusCode::NO_CONTENT)
}
#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct Content {
    release_id: Uuid,
    ttl_seconds: Option<u64>,
}
async fn content(
    State(s): State<SitesService>,
    Path((project, site)): Path<(Uuid, Uuid)>,
    Json(input): Json<Content>,
) -> Result<Json<Value>> {
    ready(&s, project, site).await?;
    let body = match input.ttl_seconds {
        Some(ttl) => json!({"ttl_seconds":ttl}),
        None => json!({}),
    };
    Ok(Json(
        s.client
            .call(
                reqwest::Method::POST,
                &format!(
                    "/internal/sites/{site}/releases/{}/content-access",
                    input.release_id
                ),
                Some(&body),
            )
            .await?,
    ))
}
async fn ready(s: &SitesService, project: Uuid, site: Uuid) -> Result<()> {
    if s.site(project, site).await?["desired_status"] != "ready" {
        return Err(Error::Conflict);
    }
    Ok(())
}
#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct EndpointRequest {
    method: String,
    path: String,
    #[serde(default)]
    query: std::collections::BTreeMap<String, String>,
    #[serde(default)]
    body: Value,
}
fn timeout() -> u64 {
    10000
}
#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct Invoke {
    id: Uuid,
    release_id: Option<Uuid>,
    request: EndpointRequest,
    #[serde(default = "timeout")]
    timeout_ms: u64,
}
async fn invoke(
    State(s): State<SitesService>,
    Path((project, site)): Path<(Uuid, Uuid)>,
    Json(input): Json<Invoke>,
) -> Result<Json<Value>> {
    ready(&s, project, site).await?;
    let body = serde_json::to_value(&input).map_err(|_| invalid("Invalid invocation."))?;
    sqlx::query(
        "insert into site_invocations(site_id,id,request) values($1,$2,$3) on conflict do nothing",
    )
    .bind(site)
    .bind(input.id)
    .bind(&body)
    .execute(&s.pool)
    .await?;
    let saved: Value =
        sqlx::query_scalar("select request from site_invocations where site_id=$1 and id=$2")
            .bind(site)
            .bind(input.id)
            .fetch_one(&s.pool)
            .await?;
    if body != saved {
        return Err(Error::Conflict);
    }
    Ok(Json(
        s.client
            .call(
                reqwest::Method::POST,
                &format!("/internal/sites/{site}/invocations"),
                Some(&body),
            )
            .await?,
    ))
}
async fn inspect(
    State(s): State<SitesService>,
    Path((project, site, invocation)): Path<(Uuid, Uuid, Uuid)>,
) -> Result<Json<Value>> {
    s.site(project, site).await?;
    Ok(Json(
        s.client
            .call(
                reqwest::Method::GET,
                &format!("/internal/sites/{site}/invocations/{invocation}"),
                None,
            )
            .await?,
    ))
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Capability {
    site_id: Uuid,
    project_id: Uuid,
    invocation_id: Uuid,
    release_id: Uuid,
    method: String,
    args: Value,
}
async fn capability(
    State(s): State<SitesService>,
    Json(input): Json<Capability>,
) -> Result<Json<Value>> {
    ready(&s, input.project_id, input.site_id).await?;
    let request: Value =
        sqlx::query_scalar("select request from site_invocations where site_id=$1 and id=$2")
            .bind(input.site_id)
            .bind(input.invocation_id)
            .fetch_optional(&s.pool)
            .await?
            .ok_or(Error::Unauthorized)?;
    if !request["release_id"].is_null() && request["release_id"] != input.release_id.to_string() {
        return Err(Error::Unauthorized);
    }
    let scope = crate::runtime::SiteScope {
        site: input.site_id,
        project: input.project_id,
        invocation: input.invocation_id,
        release: input.release_id,
    };
    let result = s
        .runtime
        .site_capability(scope, &input.method, input.args, &s.providers)
        .await?;
    Ok(Json(result))
}

async fn callback_list(
    State(s): State<SitesService>,
    Path((project, site)): Path<(Uuid, Uuid)>,
    Query(input): Query<super::callbacks::List>,
) -> Result<Json<Value>> {
    s.site(project, site).await?;
    Ok(Json(super::callbacks::list(&s.pool, site, input).await?))
}
async fn callback_get(
    State(s): State<SitesService>,
    Path((project, site, callback)): Path<(Uuid, Uuid, Uuid)>,
) -> Result<Json<Value>> {
    s.site(project, site).await?;
    Ok(Json(super::callbacks::get(&s.pool, site, callback).await?))
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
struct RetryCallback {
    expected_version: i64,
}
async fn callback_retry(
    State(s): State<SitesService>,
    Path((project, site, callback)): Path<(Uuid, Uuid, Uuid)>,
    Json(input): Json<RetryCallback>,
) -> Result<Json<Value>> {
    ready(&s, project, site).await?;
    let changed=sqlx::query("update site_callback_deliveries d set status='pending',attempts=0,next_attempt_at=clock_timestamp(),finished_at=null,version=version+1 from site_callbacks c where c.id=d.callback_id and c.site_id=$1 and c.id=$2 and d.status='failed' and d.version=$3")
        .bind(site).bind(callback).bind(input.expected_version).execute(&s.pool).await?;
    if changed.rows_affected() == 0 {
        return Err(Error::Conflict);
    }
    Ok(Json(super::callbacks::get(&s.pool, site, callback).await?))
}

async fn source(
    State(s): State<SitesService>,
    Path((project, site)): Path<(Uuid, Uuid)>,
) -> Result<Json<Value>> {
    s.site(project, site).await?;
    Ok(Json(
        s.client
            .call(
                reqwest::Method::GET,
                &format!("/internal/sites/{site}/source"),
                None,
            )
            .await?,
    ))
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Edit {
    id: Uuid,
    patch: String,
}
async fn edit(
    State(s): State<SitesService>,
    Path((project, site)): Path<(Uuid, Uuid)>,
    Json(input): Json<Edit>,
) -> Result<Json<Value>> {
    ready(&s, project, site).await?;
    Ok(Json(
        s.client
            .call(
                reqwest::Method::POST,
                &format!("/internal/sites/{site}/authoring"),
                Some(&json!({"id":input.id,"type":"patch","patch":input.patch})),
            )
            .await?,
    ))
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct SnapshotInput {
    id: Uuid,
    name: String,
}
async fn snapshot(
    State(s): State<SitesService>,
    Path((project, site)): Path<(Uuid, Uuid)>,
    Json(input): Json<SnapshotInput>,
) -> Result<Json<Value>> {
    ready(&s, project, site).await?;
    Ok(Json(
        s.client
            .call(
                reqwest::Method::POST,
                &format!("/internal/sites/{site}/authoring"),
                Some(&json!({"id":input.id,"type":"snapshot","name":input.name})),
            )
            .await?,
    ))
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct SnapshotPage {
    after: Option<Uuid>,
    limit: Option<u32>,
}
async fn snapshots(
    State(s): State<SitesService>,
    Path((project, site)): Path<(Uuid, Uuid)>,
    Query(q): Query<SnapshotPage>,
) -> Result<Json<Value>> {
    s.site(project, site).await?;
    let limit = q.limit.unwrap_or(20);
    if !(1..=50).contains(&limit) {
        return Err(invalid("Invalid snapshot page size."));
    }
    let after = q.after.map(|id| format!("&after={id}")).unwrap_or_default();
    Ok(Json(
        s.client
            .call(
                reqwest::Method::GET,
                &format!("/internal/sites/{site}/snapshots?limit={limit}{after}"),
                None,
            )
            .await?,
    ))
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Restore {
    id: Uuid,
}
async fn restore(
    State(s): State<SitesService>,
    Path((project, site, snapshot)): Path<(Uuid, Uuid, Uuid)>,
    Json(input): Json<Restore>,
) -> Result<Json<Value>> {
    ready(&s, project, site).await?;
    Ok(Json(
        s.client
            .call(
                reqwest::Method::POST,
                &format!("/internal/sites/{site}/authoring"),
                Some(&json!({"id":input.id,"type":"restore","snapshot_id":snapshot})),
            )
            .await?,
    ))
}
async fn authoring_operation(
    State(s): State<SitesService>,
    Path((project, site, operation)): Path<(Uuid, Uuid, Uuid)>,
) -> Result<Json<Value>> {
    s.site(project, site).await?;
    Ok(Json(
        s.client
            .call(
                reqwest::Method::GET,
                &format!("/internal/sites/{site}/authoring/{operation}"),
                None,
            )
            .await?,
    ))
}

/// Recent authoring conversations, including an explicitly selected site before
/// its first authoring call. The project credential and site membership are checked.
async fn authoring_sessions(
    State(s): State<SitesService>,
    Path((project, site)): Path<(Uuid, Uuid)>,
) -> Result<Json<Value>> {
    s.site(project, site).await?;
    let items:Vec<Value>=sqlx::query_scalar("select jsonb_build_object('id',ss.id,'title',ss.title,'created_at',ss.created_at,'active_run', (select jsonb_build_object('id',r.id,'status',r.status) from runs r where r.session_id=ss.id and r.status in ('ready','running','waiting'))) from sessions ss where ss.project_id=$1 and ss.harness_id='sites' and ss.archived_at is null and (exists(select 1 from site_authoring_bindings b where b.session_id=ss.id and b.site_id=$2) or ss.config->>'siteId'=$2::text) order by ss.created_at desc,ss.id desc limit 20")
        .bind(project).bind(site).fetch_all(&s.pool).await?;
    Ok(Json(json!({"items":items})))
}

async fn diagnostics(
    State(s): State<SitesService>,
    Path((project, site)): Path<(Uuid, Uuid)>,
) -> Result<Json<Value>> {
    s.site(project, site).await?;
    Ok(Json(
        s.client
            .call(
                reqwest::Method::GET,
                &format!("/internal/sites/{site}/authoring-logs"),
                None,
            )
            .await?,
    ))
}
