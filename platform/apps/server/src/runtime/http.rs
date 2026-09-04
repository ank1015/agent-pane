use super::{
    RuntimeService,
    error::{Result, RuntimeError},
    model::*,
    receipts::Reply,
};
use axum::{
    Json, Router,
    extract::Request,
    extract::{
        DefaultBodyLimit, Path, Query, State,
        rejection::{JsonRejection, PathRejection, QueryRejection},
    },
    http::{HeaderMap, HeaderValue},
    middleware::{self, Next},
    response::Response,
    routing::{get, post},
};
use serde::de::DeserializeOwned;
use serde_json::Value;
use uuid::Uuid;

pub fn router(service: RuntimeService) -> Router {
    Router::new()
        .route("/api/harnesses", get(harnesses))
        .route("/api/harnesses/{id}", get(harness))
        .route(
            "/api/projects/{id}/sessions",
            get(sessions).post(create_session),
        )
        .route("/api/sessions/{id}", get(session).patch(patch_session))
        .route("/api/sessions/{id}/messages", get(messages))
        .route("/api/sessions/{id}/forks", post(fork))
        .route("/api/sessions/{id}/runs", get(runs).post(start_run))
        .route("/api/runs/{id}", get(run))
        .route("/api/runs/{id}/children", get(children))
        .route("/api/runs/{id}/inputs", get(inputs).post(submit_input))
        .route("/api/runs/{id}/abort", post(abort))
        .route("/api/runs/{id}/waits", get(waits))
        .route("/api/runs/{id}/events", get(events))
        .route("/api/runs/{id}/events/stream", get(super::stream::events))
        .layer(DefaultBodyLimit::max(1024 * 1024))
        .layer(middleware::from_fn(private_responses))
        .with_state(service)
}
pub(super) async fn private_responses(request: Request, next: Next) -> Response {
    let mut response = next.run(request).await;
    response
        .headers_mut()
        .entry("cache-control")
        .or_insert(HeaderValue::from_static("no-store"));
    response
}
pub(super) fn id(path: std::result::Result<Path<Uuid>, PathRejection>) -> Result<Uuid> {
    path.map(|Path(id)| id)
        .map_err(|_| RuntimeError::Invalid("Provide a valid resource UUID."))
}
pub(super) fn query<T: DeserializeOwned>(
    value: std::result::Result<Query<T>, QueryRejection>,
) -> Result<T> {
    value
        .map(|Query(value)| value)
        .map_err(|_| RuntimeError::Invalid("Invalid query parameters."))
}
pub(super) fn body<T>(value: std::result::Result<Json<T>, JsonRejection>) -> Result<T> {
    value
        .map(|Json(value)| value)
        .map_err(|error| RuntimeError::Body(error.status()))
}
pub(super) fn key(headers: &HeaderMap) -> Result<&str> {
    let mut values = headers.get_all("idempotency-key").iter();
    let key = values
        .next()
        .and_then(|v| v.to_str().ok())
        .ok_or(RuntimeError::Invalid(
            "Provide an Idempotency-Key header for POST requests.",
        ))?;
    if values.next().is_some()
        || key.is_empty()
        || key.len() > 256
        || key.bytes().any(|b| !(33..=126).contains(&b))
    {
        return Err(RuntimeError::Invalid(
            "Idempotency-Key must contain 1–256 printable ASCII characters without spaces.",
        ));
    }
    Ok(key)
}
type Id = std::result::Result<Path<Uuid>, PathRejection>;
type Params<T> = std::result::Result<Query<T>, QueryRejection>;
type Body<T> = std::result::Result<Json<T>, JsonRejection>;
async fn harnesses(State(s): State<RuntimeService>) -> Result<Json<Value>> {
    Ok(Json(s.harnesses().await?))
}
async fn harness(State(s): State<RuntimeService>, Path(id): Path<String>) -> Result<Json<Value>> {
    Ok(Json(s.harness(&id).await?))
}
async fn sessions(
    State(s): State<RuntimeService>,
    p: Id,
    q: Params<ListQuery>,
) -> Result<Json<Value>> {
    Ok(Json(s.sessions(id(p)?, query(q)?).await?))
}
async fn session(State(s): State<RuntimeService>, p: Id) -> Result<Json<Value>> {
    Ok(Json(s.session(id(p)?).await?))
}
async fn run(State(s): State<RuntimeService>, p: Id) -> Result<Json<Value>> {
    Ok(Json(s.run(id(p)?).await?))
}
async fn messages(
    State(s): State<RuntimeService>,
    p: Id,
    q: Params<MessageQuery>,
) -> Result<Json<Value>> {
    Ok(Json(s.messages(id(p)?, query(q)?).await?))
}
async fn runs(State(s): State<RuntimeService>, p: Id, q: Params<ListQuery>) -> Result<Json<Value>> {
    Ok(Json(s.runs(id(p)?, false, query(q)?).await?))
}
async fn children(
    State(s): State<RuntimeService>,
    p: Id,
    q: Params<ListQuery>,
) -> Result<Json<Value>> {
    Ok(Json(s.runs(id(p)?, true, query(q)?).await?))
}
async fn inputs(
    State(s): State<RuntimeService>,
    p: Id,
    q: Params<SequenceQuery>,
) -> Result<Json<Value>> {
    Ok(Json(s.inputs(id(p)?, query(q)?).await?))
}
async fn events(
    State(s): State<RuntimeService>,
    p: Id,
    q: Params<SequenceQuery>,
) -> Result<Json<Value>> {
    Ok(Json(s.events(id(p)?, query(q)?).await?))
}
async fn waits(
    State(s): State<RuntimeService>,
    p: Id,
    q: Params<ListQuery>,
) -> Result<Json<Value>> {
    Ok(Json(s.waits(id(p)?, query(q)?).await?))
}
async fn create_session(
    State(s): State<RuntimeService>,
    p: Id,
    h: HeaderMap,
    b: Body<CreateSession>,
) -> Result<Reply> {
    s.create_session(id(p)?, key(&h)?, body(b)?).await
}
async fn patch_session(
    State(s): State<RuntimeService>,
    p: Id,
    b: Body<PatchSession>,
) -> Result<Json<Value>> {
    Ok(Json(s.patch_session(id(p)?, body(b)?).await?))
}
async fn fork(
    State(s): State<RuntimeService>,
    p: Id,
    h: HeaderMap,
    b: Body<ForkSession>,
) -> Result<Reply> {
    s.fork(id(p)?, key(&h)?, body(b)?).await
}
async fn start_run(
    State(s): State<RuntimeService>,
    p: Id,
    h: HeaderMap,
    b: Body<StartRun>,
) -> Result<Reply> {
    s.start_run(id(p)?, key(&h)?, body(b)?).await
}
async fn submit_input(
    State(s): State<RuntimeService>,
    p: Id,
    h: HeaderMap,
    b: Body<SubmitInput>,
) -> Result<Reply> {
    s.submit_input(id(p)?, key(&h)?, body(b)?).await
}
async fn abort(
    State(s): State<RuntimeService>,
    p: Id,
    h: HeaderMap,
    b: Body<AbortRun>,
) -> Result<Reply> {
    s.abort(id(p)?, key(&h)?, body(b)?).await
}
