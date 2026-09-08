use std::{sync::Arc, time::Instant};

use axum::{
    Json, Router,
    extract::{
        DefaultBodyLimit, MatchedPath, Path, Request, State,
        rejection::{JsonRejection, PathRejection},
    },
    http::{HeaderValue, StatusCode},
    middleware::{self, Next},
    response::{IntoResponse, Response},
    routing::{get, put},
};
use serde::Deserialize;
use serde_json::json;
use sha2::{Digest, Sha256};
use subtle::ConstantTimeEq;
use tokio::sync::Semaphore;
use uuid::Uuid;

use crate::{DesiredStatus, Error, Result, SiteService, Status, config::validate_token};

#[derive(Clone)]
pub(crate) struct App {
    pub service: SiteService,
    token_hash: [u8; 32],
    capacity: Arc<Semaphore>,
    pub uploads: Arc<Semaphore>,
    pub content: Option<crate::content::ContentHost>,
}

pub fn router(service: SiteService, token: &str) -> Result<Router> {
    router_with_content(service, token, None)
}

pub fn router_with_content(
    service: SiteService,
    token: &str,
    content: Option<crate::content::ContentHost>,
) -> Result<Router> {
    validate_token(token)?;
    let app = App {
        service,
        token_hash: Sha256::digest(token.as_bytes()).into(),
        capacity: Arc::new(Semaphore::new(64)),
        uploads: Arc::new(Semaphore::new(2)),
        content,
    };
    Ok(Router::new()
        .route("/healthz", get(|| async { Json(json!({"status":"ok"})) }))
        .route("/readyz", get(ready))
        .route(
            "/internal/sites/{id}",
            put(provision).get(inspect).patch(patch),
        )
        .merge(crate::bundle_http::routes(app.clone()))
        .merge(crate::backend_http::routes())
        .merge(crate::authoring_http::routes())
        .fallback(|| async {
            (
                StatusCode::NOT_FOUND,
                Json(json!({"error":{"code":"NOT_FOUND","message":"Route not found."}})),
            )
        })
        .method_not_allowed_fallback(|| async {
            (
                StatusCode::METHOD_NOT_ALLOWED,
                Json(
                    json!({"error":{"code":"METHOD_NOT_ALLOWED","message":"Method not allowed."}}),
                ),
            )
        })
        .layer(DefaultBodyLimit::max(16 * 1024))
        .layer(middleware::from_fn_with_state(app.clone(), authenticate))
        .layer(middleware::from_fn(audit))
        .with_state(app))
}

async fn authenticate(State(app): State<App>, request: Request, next: Next) -> Response {
    let mut headers = request.headers().get_all("authorization").iter();
    let token = headers
        .next()
        .and_then(|h| h.to_str().ok())
        .and_then(|h| h.strip_prefix("Bearer "));
    let authorized = headers.next().is_none()
        && token.is_some_and(|token| {
            token.len() <= 256
                && bool::from(app.token_hash.ct_eq(&Sha256::digest(token.as_bytes())))
        });
    if !authorized {
        return Error::Unauthorized.into_response();
    }
    let release_listing = request.method() == axum::http::Method::GET
        && request.extensions().get::<MatchedPath>().is_some_and(|p| {
            matches!(
                p.as_str(),
                "/internal/sites/{id}/releases" | "/internal/sites/{id}/snapshots"
            )
        });
    if request.uri().query().is_some() && !release_listing {
        return Error::Invalid("Query parameters are not supported.").into_response();
    }
    let Ok(_permit) = app.capacity.try_acquire() else {
        return (StatusCode::SERVICE_UNAVAILABLE, Json(json!({"error":{"code":"CAPACITY_EXCEEDED","message":"Too many concurrent requests."}}))).into_response();
    };
    next.run(request).await
}

pub(crate) async fn audit(request: Request, next: Next) -> Response {
    let started = Instant::now();
    let request_id = Uuid::new_v4().to_string();
    let method = request.method().clone();
    let route = request
        .extensions()
        .get::<MatchedPath>()
        .map(|p| p.as_str().to_owned())
        .unwrap_or_else(|| "unmatched".into());
    let mut response = next.run(request).await;
    let headers = response.headers_mut();
    headers.insert(
        "x-request-id",
        HeaderValue::from_str(&request_id).expect("UUID is a header value"),
    );
    if !headers.contains_key("cache-control") {
        headers.insert("cache-control", HeaderValue::from_static("no-store"));
    }
    headers.insert(
        "x-content-type-options",
        HeaderValue::from_static("nosniff"),
    );
    headers.insert("referrer-policy", HeaderValue::from_static("no-referrer"));
    if response.status() == StatusCode::SERVICE_UNAVAILABLE {
        response
            .headers_mut()
            .insert("retry-after", HeaderValue::from_static("5"));
    }
    tracing::info!(%request_id, %method, %route, status = response.status().as_u16(), elapsed_ms = started.elapsed().as_millis() as u64, "sites request");
    response
}

async fn ready(State(app): State<App>) -> Result<Json<serde_json::Value>> {
    app.service.ready().await?;
    Ok(Json(json!({"status":"ready"})))
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Provision {
    project_id: Uuid,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Patch {
    status: DesiredStatus,
}

fn site_id(path: std::result::Result<Path<Uuid>, PathRejection>) -> Result<Uuid> {
    path.map(|Path(id)| id)
        .map_err(|_| Error::Invalid("Site ID must be a UUID."))
}

fn payload<T>(
    body: std::result::Result<Json<T>, JsonRejection>,
) -> std::result::Result<T, (StatusCode, Json<serde_json::Value>)> {
    body.map(|Json(value)| value).map_err(|error| {
        (error.status(), Json(json!({"error":{"code":"INVALID_BODY","message":"Provide a valid JSON object with only the documented fields."}})))
    })
}

async fn provision(
    State(app): State<App>,
    path: std::result::Result<Path<Uuid>, PathRejection>,
    body: std::result::Result<Json<Provision>, JsonRejection>,
) -> Response {
    let id = match site_id(path) {
        Ok(id) => id,
        Err(error) => return error.into_response(),
    };
    let body = match payload(body) {
        Ok(body) => body,
        Err(response) => return response.into_response(),
    };
    match app.service.provision(id, body.project_id).await {
        Ok(site) => {
            let status = if matches!(site.status, Status::Provisioning | Status::Failed) {
                StatusCode::ACCEPTED
            } else {
                StatusCode::OK
            };
            (status, Json(site)).into_response()
        }
        Err(error) => error.into_response(),
    }
}

async fn inspect(
    State(app): State<App>,
    path: std::result::Result<Path<Uuid>, PathRejection>,
) -> Result<Json<crate::Site>> {
    Ok(Json(app.service.get(site_id(path)?).await?))
}

async fn patch(
    State(app): State<App>,
    path: std::result::Result<Path<Uuid>, PathRejection>,
    body: std::result::Result<Json<Patch>, JsonRejection>,
) -> Response {
    let id = match site_id(path) {
        Ok(id) => id,
        Err(error) => return error.into_response(),
    };
    let body = match payload(body) {
        Ok(body) => body,
        Err(response) => return response.into_response(),
    };
    match app.service.set_status(id, body.status).await {
        Ok(site) => {
            let status = if matches!(site.status, Status::Provisioning | Status::Failed) {
                StatusCode::ACCEPTED
            } else {
                StatusCode::OK
            };
            (status, Json(site)).into_response()
        }
        Err(error) => error.into_response(),
    }
}
