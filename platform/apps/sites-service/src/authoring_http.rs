use crate::{Error, Result, http::App};
use axum::{
    Json, Router,
    extract::{DefaultBodyLimit, Path, Query, State},
    routing::{get, post},
};
use platform_runtime_contracts::sites_authoring::Operation;
use serde::Deserialize;
use serde_json::Value;
use uuid::Uuid;
pub(crate) fn routes() -> Router<App> {
    Router::new()
        .route("/internal/sites/{id}/source", get(source))
        .route("/internal/sites/{id}/authoring-logs", get(logs))
        .route("/internal/sites/{id}/authoring", post(author))
        .route("/internal/sites/{id}/authoring/{operation}", get(operation))
        .route("/internal/sites/{id}/snapshots", get(snapshots))
        .route("/internal/sites/{id}/sql/query", post(query))
        .route("/internal/sites/{id}/sql/{operation}", post(execute))
        .layer(DefaultBodyLimit::max(384 * 1024))
}
async fn source(State(s): State<App>, Path(id): Path<Uuid>) -> Result<Json<Value>> {
    Ok(Json(
        serde_json::to_value(s.service.authoring_source(id).await?).map_err(|_| Error::Storage)?,
    ))
}
async fn author(
    State(s): State<App>,
    Path(id): Path<Uuid>,
    Json(input): Json<Operation>,
) -> Result<Json<Value>> {
    Ok(Json(
        s.service
            .author(id, input, &std::env::current_exe()?)
            .await?,
    ))
}
async fn operation(
    State(s): State<App>,
    Path((site, id)): Path<(Uuid, Uuid)>,
) -> Result<Json<Value>> {
    Ok(Json(s.service.authoring_operation(site, id).await?))
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Page {
    after: Option<Uuid>,
    limit: Option<u32>,
}
async fn snapshots(
    State(s): State<App>,
    Path(id): Path<Uuid>,
    Query(q): Query<Page>,
) -> Result<Json<Value>> {
    Ok(Json(
        s.service
            .snapshots(id, q.after, q.limit.unwrap_or(20))
            .await?,
    ))
}

async fn query(
    State(s): State<App>,
    Path(site): Path<Uuid>,
    Json(input): Json<platform_runtime_contracts::sites_authoring::Sql>,
) -> Result<Json<Value>> {
    Ok(Json(s.service.authoring_sql(site, None, input).await?))
}
async fn execute(
    State(s): State<App>,
    Path((site, id)): Path<(Uuid, Uuid)>,
    Json(input): Json<platform_runtime_contracts::sites_authoring::Sql>,
) -> Result<Json<Value>> {
    Ok(Json(s.service.authoring_sql(site, Some(id), input).await?))
}

async fn logs(State(s): State<App>, Path(site): Path<Uuid>) -> Result<Json<Value>> {
    s.service.get(site).await?;
    let rows:Vec<(String,String,String,Option<String>,String)>=sqlx::query_as("SELECT id,release_id,status,error_code,created_at FROM invocations WHERE site_id=? ORDER BY created_at DESC,id DESC LIMIT 20").bind(site.to_string()).fetch_all(&s.service.0.pool).await?;
    Ok(Json(
        serde_json::json!({"items":rows.into_iter().map(|(id,release,status,error,created)|serde_json::json!({"id":id,"releaseId":release,"status":status,"errorCode":error,"createdAt":created})).collect::<Vec<_>>(),"limit":20}),
    ))
}
