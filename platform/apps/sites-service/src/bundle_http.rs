use axum::{
    Json, Router,
    body::Body,
    extract::{
        DefaultBodyLimit, Path, Query, Request, State,
        rejection::{JsonRejection, PathRejection, QueryRejection},
    },
    http::{HeaderMap, HeaderValue, StatusCode},
    middleware::{self, Next},
    response::{IntoResponse, Response},
    routing::{get, post, put},
};
use serde::Deserialize;
use serde_json::json;
use uuid::Uuid;

use crate::{
    Error, Result,
    bundles::{Activation, Kind, ReleaseUpload, RevisionUpload, UPLOAD_BODY_LIMIT},
    http::App,
};

pub(crate) fn routes(app: App) -> Router<App> {
    let uploads = Router::new()
        .route("/internal/sites/{id}/revisions", post(revision))
        .route("/internal/sites/{id}/releases", post(release))
        .layer(DefaultBodyLimit::max(UPLOAD_BODY_LIMIT))
        .layer(middleware::from_fn_with_state(app, upload_capacity));
    uploads
        .route("/internal/sites/{id}/revisions/{bundle}", get(get_revision))
        .route(
            "/internal/sites/{id}/revisions/{bundle}/files/{*path}",
            get(source_file),
        )
        .route("/internal/sites/{id}/releases", get(list))
        .route("/internal/sites/{id}/releases/{bundle}", get(get_release))
        .route("/internal/sites/{id}/active-release", put(activate))
        .route(
            "/internal/sites/{id}/releases/{bundle}/content-access",
            post(access),
        )
}

async fn upload_capacity(State(app): State<App>, request: Request, next: Next) -> Response {
    let Ok(_permit) = app.uploads.try_acquire() else {
        return Error::Storage.into_response();
    };
    next.run(request).await
}

fn path<T>(input: std::result::Result<Path<T>, PathRejection>) -> Result<T> {
    input
        .map(|Path(value)| value)
        .map_err(|_| Error::Invalid("Invalid site or bundle path."))
}
fn body<T>(input: std::result::Result<Json<T>, JsonRejection>) -> Result<T> {
    input
        .map(|Json(value)| value)
        .map_err(|error| Error::Body(error.status()))
}

async fn revision(
    State(app): State<App>,
    id: std::result::Result<Path<Uuid>, PathRejection>,
    input: std::result::Result<Json<RevisionUpload>, JsonRejection>,
) -> Result<Response> {
    Ok((
        StatusCode::OK,
        Json(app.service.upload_revision(path(id)?, body(input)?).await?),
    )
        .into_response())
}
async fn release(
    State(app): State<App>,
    id: std::result::Result<Path<Uuid>, PathRejection>,
    input: std::result::Result<Json<ReleaseUpload>, JsonRejection>,
) -> Result<Response> {
    Ok((
        StatusCode::OK,
        Json(app.service.upload_release(path(id)?, body(input)?).await?),
    )
        .into_response())
}
async fn get_revision(
    State(app): State<App>,
    ids: std::result::Result<Path<(Uuid, Uuid)>, PathRejection>,
) -> Result<Response> {
    let (site, id) = path(ids)?;
    Ok(Json(app.service.bundle(site, Kind::Revision, id).await?).into_response())
}
async fn get_release(
    State(app): State<App>,
    ids: std::result::Result<Path<(Uuid, Uuid)>, PathRejection>,
) -> Result<Response> {
    let (site, id) = path(ids)?;
    Ok(Json(app.service.bundle(site, Kind::Release, id).await?).into_response())
}
async fn source_file(
    State(app): State<App>,
    ids: std::result::Result<Path<(Uuid, Uuid, String)>, PathRejection>,
) -> Result<Response> {
    let (site, id, path) = path(ids)?;
    let bytes = app
        .service
        .read_bundle_file(site, Kind::Revision, id, path)
        .await?;
    let mut response = Body::from(bytes).into_response();
    response.headers_mut().insert(
        "content-type",
        HeaderValue::from_static("application/octet-stream"),
    );
    response.headers_mut().insert(
        "content-disposition",
        HeaderValue::from_static("attachment"),
    );
    response.headers_mut().insert(
        "content-security-policy",
        HeaderValue::from_static("default-src 'none'; sandbox"),
    );
    Ok(response)
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ListQuery {
    after: Option<Uuid>,
    limit: Option<u32>,
}

async fn list(
    State(app): State<App>,
    site: std::result::Result<Path<Uuid>, PathRejection>,
    query: std::result::Result<Query<ListQuery>, QueryRejection>,
) -> Result<Response> {
    let Query(query) = query.map_err(|_| Error::Invalid("Invalid release listing query."))?;
    let limit = query.limit.unwrap_or(50);
    let items = app
        .service
        .list_releases(path(site)?, query.after, limit)
        .await?;
    let next_after = if items.len() == limit as usize {
        items.last().map(|b| b.descriptor.id)
    } else {
        None
    };
    Ok(Json(json!({"items":items,"next_after":next_after})).into_response())
}

async fn activate(
    State(app): State<App>,
    site: std::result::Result<Path<Uuid>, PathRejection>,
    headers: HeaderMap,
    input: std::result::Result<Json<Activation>, JsonRejection>,
) -> Result<Response> {
    let mut keys = headers.get_all("idempotency-key").iter();
    let key = keys
        .next()
        .and_then(|h| h.to_str().ok())
        .ok_or(Error::Invalid("Idempotency-Key is required."))?;
    if keys.next().is_some() {
        return Err(Error::Invalid("Only one Idempotency-Key is allowed."));
    }
    Ok(Json(
        app.service
            .activate_release(path(site)?, key.into(), body(input)?)
            .await?,
    )
    .into_response())
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct AccessRequest {
    ttl_seconds: Option<u64>,
}

async fn access(
    State(app): State<App>,
    ids: std::result::Result<Path<(Uuid, Uuid)>, PathRejection>,
    input: std::result::Result<Json<AccessRequest>, JsonRejection>,
) -> Result<Response> {
    let (site, release) = path(ids)?;
    let input = body(input)?;
    let host = app.content.ok_or(Error::Conflict(
        "CONTENT_HOST_DISABLED",
        "Configure the separate content listener first.",
    ))?;
    Ok(Json(
        host.issue(
            &app.service,
            site,
            release,
            input.ttl_seconds.unwrap_or(900),
        )
        .await?,
    )
    .into_response())
}
