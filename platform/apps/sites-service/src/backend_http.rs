use crate::{Error, Result, backend::Invoke, http::App};
use axum::{
    Json, Router,
    body::Body,
    extract::{
        DefaultBodyLimit, Path, State,
        rejection::{JsonRejection, PathRejection},
    },
    response::{IntoResponse, Response},
    routing::{get, post, put},
};
use serde::Deserialize;
use serde_json::{Value, json};
use uuid::Uuid;

pub(crate) fn routes() -> Router<App> {
    Router::new()
        .route("/internal/sites/{id}/invocations", post(invoke).get(list))
        .route(
            "/internal/sites/{id}/invocations/{invocation}",
            get(inspect),
        )
        .route("/internal/sites/{id}/schema", get(schema))
        .route("/internal/sites/{id}/schema/migrations", post(migrate))
        .route(
            "/internal/sites/{id}/backups/{backup}",
            put(backup).get(download),
        )
        .layer(DefaultBodyLimit::max(256 * 1024))
}
fn path<T>(value: std::result::Result<Path<T>, PathRejection>) -> Result<T> {
    value
        .map(|Path(v)| v)
        .map_err(|_| Error::Invalid("Invalid UUID path."))
}
fn body<T>(value: std::result::Result<Json<T>, JsonRejection>) -> Result<T> {
    value.map(|Json(v)| v).map_err(|e| Error::Body(e.status()))
}
async fn invoke(
    State(app): State<App>,
    id: std::result::Result<Path<Uuid>, PathRejection>,
    input: std::result::Result<Json<Invoke>, JsonRejection>,
) -> Result<Response> {
    Ok(Json(app.service.invoke(path(id)?, body(input)?).await?).into_response())
}
async fn inspect(
    State(app): State<App>,
    ids: std::result::Result<Path<(Uuid, Uuid)>, PathRejection>,
) -> Result<Response> {
    let (site, id) = path(ids)?;
    Ok(Json(app.service.invocation(site, id).await?).into_response())
}
async fn list(
    State(app): State<App>,
    id: std::result::Result<Path<Uuid>, PathRejection>,
) -> Result<Json<Value>> {
    Ok(Json(
        json!({"items":app.service.invocations(path(id)?).await?,"limit":100}),
    ))
}
async fn schema(
    State(app): State<App>,
    id: std::result::Result<Path<Uuid>, PathRejection>,
) -> Result<Json<Value>> {
    Ok(Json(app.service.schema(path(id)?).await?))
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Migrate {
    release_id: Uuid,
}
async fn migrate(
    State(app): State<App>,
    id: std::result::Result<Path<Uuid>, PathRejection>,
    input: std::result::Result<Json<Migrate>, JsonRejection>,
) -> Result<Json<Value>> {
    Ok(Json(
        app.service
            .apply_migrations(path(id)?, body(input)?.release_id)
            .await?,
    ))
}
async fn backup(
    State(app): State<App>,
    ids: std::result::Result<Path<(Uuid, Uuid)>, PathRejection>,
) -> Result<Json<Value>> {
    let (site, id) = path(ids)?;
    Ok(Json(app.service.backup(site, id).await?))
}
async fn download(
    State(app): State<App>,
    ids: std::result::Result<Path<(Uuid, Uuid)>, PathRejection>,
) -> Result<Response> {
    let (site, id) = path(ids)?;
    let path = app.service.backup_file(site, id).await?;
    let file = tokio::fs::File::open(path).await?;
    Ok((
        [
            ("content-type", "application/vnd.sqlite3"),
            ("content-disposition", "attachment; filename=site.sqlite"),
        ],
        Body::from_stream(tokio_util::io::ReaderStream::new(file)),
    )
        .into_response())
}
