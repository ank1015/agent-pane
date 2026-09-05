use super::{
    RuntimeService,
    admin::{Harness, WorkerQuery},
    error::{Result, RuntimeError},
    http::{body, id, private_responses, query},
    workers::{token_hash, token_matches},
};
use axum::{
    Json, Router,
    extract::{
        DefaultBodyLimit, Path, Query, Request, State,
        rejection::{JsonRejection, PathRejection, QueryRejection},
    },
    middleware::{self, Next},
    response::{IntoResponse, Response},
    routing::{get, put},
};
use serde_json::{Map, Value};
use uuid::Uuid;

#[derive(Clone)]
struct Auth {
    hash: Vec<u8>,
    enabled: bool,
}

/// Separate admin credentials; absent or malformed credentials fail closed.
pub fn admin_router(service: RuntimeService, admin_token: impl AsRef<str>) -> Router {
    let token = admin_token.as_ref();
    let auth = Auth {
        hash: token_hash(token),
        enabled: (32..=256).contains(&token.len())
            && token.bytes().all(|b| (33..=126).contains(&b)),
    };
    Router::new()
        .route(
            "/internal/harnesses/{id}",
            put(put_harness).patch(patch_harness),
        )
        .route("/internal/workers", get(workers))
        .route("/internal/workers/{id}", get(worker))
        .layer(DefaultBodyLimit::max(256 * 1024))
        .layer(middleware::from_fn_with_state(auth, authenticate))
        .layer(middleware::from_fn(private_responses))
        .with_state(service)
}
async fn authenticate(State(auth): State<Auth>, request: Request, next: Next) -> Response {
    let mut values = request.headers().get_all("authorization").iter();
    let token = values
        .next()
        .and_then(|h| h.to_str().ok())
        .and_then(|h| h.strip_prefix("Bearer "));
    if !auth.enabled
        || values.next().is_some()
        || !token.is_some_and(|t| t.len() <= 256 && token_matches(&auth.hash, &token_hash(t)))
    {
        return RuntimeError::AdminUnauthorized.into_response();
    }
    next.run(request).await
}
type Body<T> = std::result::Result<Json<T>, JsonRejection>;
type HarnessId = std::result::Result<Path<String>, PathRejection>;
fn harness_id(p: HarnessId) -> Result<String> {
    p.map(|Path(id)| id)
        .map_err(|_| RuntimeError::Invalid("Provide a valid harness id."))
}
async fn put_harness(
    State(s): State<RuntimeService>,
    p: HarnessId,
    b: Body<Harness>,
) -> Result<Json<Value>> {
    Ok(Json(s.put_harness(&harness_id(p)?, body(b)?).await?))
}
async fn patch_harness(
    State(s): State<RuntimeService>,
    p: HarnessId,
    b: Body<Map<String, Value>>,
) -> Result<Json<Value>> {
    Ok(Json(s.patch_harness(&harness_id(p)?, body(b)?).await?))
}
async fn workers(
    State(s): State<RuntimeService>,
    q: std::result::Result<Query<WorkerQuery>, QueryRejection>,
) -> Result<Json<Value>> {
    Ok(Json(s.admin_workers(query(q)?).await?))
}
async fn worker(
    State(s): State<RuntimeService>,
    p: std::result::Result<Path<Uuid>, PathRejection>,
) -> Result<Json<Value>> {
    Ok(Json(s.admin_worker(id(p)?).await?))
}
