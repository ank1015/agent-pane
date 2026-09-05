use super::{
    RuntimeService,
    error::{Result, RuntimeError},
    http::{body, id, key, private_responses, query},
    model::SequenceQuery,
    receipts::Reply,
    worker_model::*,
    workers::{token_hash, token_matches},
};
use axum::{
    Extension, Json, Router,
    extract::{
        DefaultBodyLimit, Path, Query, Request, State,
        rejection::{JsonRejection, PathRejection, QueryRejection},
    },
    http::{HeaderMap, Method},
    middleware::{self, Next},
    response::{IntoResponse, Response},
    routing::{get, post, put},
};
use serde_json::Value;
use uuid::Uuid;

#[derive(Clone)]
struct Auth {
    service: RuntimeService,
    registration_hash: Vec<u8>,
    registration_enabled: bool,
}
/// The bootstrap token authorizes registration only. Subsequent calls use the
/// per-process worker token supplied at registration, never the bootstrap token.
/// An absent/short bootstrap token disables registration (fails closed).
pub fn worker_router(service: RuntimeService, registration_token: impl AsRef<str>) -> Router {
    let token = registration_token.as_ref();
    let auth = Auth {
        service: service.clone(),
        registration_hash: token_hash(token),
        registration_enabled: (32..=256).contains(&token.len())
            && token.bytes().all(|b| (33..=126).contains(&b)),
    };
    Router::new()
        .route("/internal/workers/{id}", put(register).patch(patch))
        .route("/internal/workers/{id}/heartbeat", post(heartbeat))
        .route("/internal/workers/{id}/claims", post(claim))
        .route("/internal/workers/{id}/assignments", get(assignments))
        .route("/internal/runs/{id}/context", get(context))
        .route(
            "/internal/runs/{id}/environments",
            get(environments).post(create_environment),
        )
        .route("/internal/runs/{id}/session-state", get(session_state))
        .route("/internal/runs/{id}/inputs", get(inputs))
        .route("/internal/runs/{id}/commits", post(commit))
        .route("/internal/runs/{id}/events", post(events))
        .route("/internal/runs/{id}/children", post(children))
        .route("/internal/runs/{id}/follow-ups", post(follow_up))
        .route("/internal/runs/{id}/messages", post(messages))
        .route("/internal/runs/{id}/abort-requests", post(abort))
        .layer(DefaultBodyLimit::max(1024 * 1024))
        .layer(middleware::from_fn_with_state(auth, authenticate))
        .layer(middleware::from_fn(private_responses))
        .with_state(service)
}
fn header<'a>(h: &'a HeaderMap, key: &str) -> Result<&'a str> {
    let mut values = h.get_all(key).iter();
    let value = values
        .next()
        .and_then(|v| v.to_str().ok())
        .ok_or(RuntimeError::Unauthorized)?;
    if values.next().is_some() {
        return Err(RuntimeError::Unauthorized);
    }
    Ok(value)
}
async fn authenticate(State(auth): State<Auth>, mut request: Request, next: Next) -> Response {
    let headers = request.headers().clone();
    let path = request.uri().path().to_owned();
    let method = request.method().clone();
    let result: Result<Uuid> = async {
        let token = header(&headers, "authorization")?
            .strip_prefix("Bearer ")
            .ok_or(RuntimeError::Unauthorized)?;
        if token.len() > 256 {
            return Err(RuntimeError::Unauthorized);
        }
        let worker = if let Some(rest) = path.strip_prefix("/internal/workers/") {
            rest.split('/')
                .next()
                .and_then(|id| id.parse().ok())
                .ok_or(RuntimeError::Unauthorized)?
        } else {
            header(&headers, "x-worker-id")?
                .parse()
                .map_err(|_| RuntimeError::Unauthorized)?
        };
        let registration = method == Method::PUT
            && path
                .strip_prefix("/internal/workers/")
                .is_some_and(|rest| !rest.contains('/'));
        if registration {
            if !auth.registration_enabled
                || !token_matches(&auth.registration_hash, &token_hash(token))
            {
                return Err(RuntimeError::Unauthorized);
            }
        } else {
            auth.service.authenticate_worker(worker, token).await?;
        }
        Ok(worker)
    }
    .await;
    match result {
        Ok(worker) => {
            request.extensions_mut().insert(worker);
            next.run(request).await
        }
        Err(error) => error.into_response(),
    }
}
fn owner(worker: Uuid, h: &HeaderMap) -> Result<Owner> {
    let epoch = header(h, "x-lease-epoch")?
        .parse::<i64>()
        .map_err(|_| RuntimeError::Invalid("Provide a positive X-Lease-Epoch."))?;
    if epoch < 1 {
        return Err(RuntimeError::Invalid("Provide a positive X-Lease-Epoch."));
    }
    Ok(Owner {
        worker_id: worker,
        lease_epoch: epoch,
    })
}
type Id = std::result::Result<Path<Uuid>, PathRejection>;
type Body<T> = std::result::Result<Json<T>, JsonRejection>;
type Params<T> = std::result::Result<Query<T>, QueryRejection>;
async fn register(
    State(s): State<RuntimeService>,
    p: Id,
    b: Body<RegisterWorker>,
) -> Result<Json<Value>> {
    Ok(Json(s.register_worker(id(p)?, body(b)?).await?))
}
async fn patch(
    State(s): State<RuntimeService>,
    p: Id,
    b: Body<PatchWorker>,
) -> Result<Json<Value>> {
    Ok(Json(s.patch_worker(id(p)?, body(b)?).await?))
}
async fn heartbeat(
    State(s): State<RuntimeService>,
    p: Id,
    b: Body<Heartbeat>,
) -> Result<Json<Value>> {
    Ok(Json(s.heartbeat(id(p)?, body(b)?).await?))
}
async fn claim(
    State(s): State<RuntimeService>,
    p: Id,
    h: HeaderMap,
    b: Body<Claim>,
) -> Result<Json<Value>> {
    Ok(Json(s.claim(id(p)?, key(&h)?, body(b)?).await?))
}
async fn assignments(State(s): State<RuntimeService>, p: Id) -> Result<Json<Value>> {
    Ok(Json(s.assignments(id(p)?).await?))
}
async fn context(
    State(s): State<RuntimeService>,
    Extension(w): Extension<Uuid>,
    p: Id,
    h: HeaderMap,
    q: Params<ContextQuery>,
) -> Result<Json<Value>> {
    Ok(Json(s.context(id(p)?, owner(w, &h)?, query(q)?).await?))
}
async fn environments(
    State(s): State<RuntimeService>,
    Extension(w): Extension<Uuid>,
    p: Id,
    h: HeaderMap,
) -> Result<Json<Value>> {
    Ok(Json(s.worker_environments(id(p)?, owner(w, &h)?).await?))
}
async fn create_environment(
    State(s): State<RuntimeService>,
    Extension(w): Extension<Uuid>,
    p: Id,
    h: HeaderMap,
    b: Body<crate::projects::environments::CreateEnvironment>,
) -> Result<Reply> {
    s.worker_create_environment(id(p)?, owner(w, &h)?, key(&h)?, body(b)?)
        .await
}
async fn inputs(
    State(s): State<RuntimeService>,
    Extension(w): Extension<Uuid>,
    p: Id,
    h: HeaderMap,
    q: Params<SequenceQuery>,
) -> Result<Json<Value>> {
    Ok(Json(
        s.worker_inputs(id(p)?, owner(w, &h)?, query(q)?).await?,
    ))
}
async fn session_state(
    State(s): State<RuntimeService>,
    Extension(w): Extension<Uuid>,
    p: Id,
    h: HeaderMap,
    q: Params<platform_runtime_contracts::SessionStateQuery>,
) -> Result<Json<Value>> {
    Ok(Json(
        s.session_state(id(p)?, owner(w, &h)?, query(q)?).await?,
    ))
}
async fn commit(
    State(s): State<RuntimeService>,
    Extension(w): Extension<Uuid>,
    p: Id,
    h: HeaderMap,
    b: Body<Commit>,
) -> Result<Reply> {
    s.commit(id(p)?, owner(w, &h)?, key(&h)?, body(b)?).await
}
async fn events(
    State(s): State<RuntimeService>,
    Extension(w): Extension<Uuid>,
    p: Id,
    h: HeaderMap,
    b: Body<Events>,
) -> Result<Reply> {
    s.worker_events(id(p)?, owner(w, &h)?, key(&h)?, body(b)?)
        .await
}
async fn children(
    State(s): State<RuntimeService>,
    Extension(w): Extension<Uuid>,
    p: Id,
    h: HeaderMap,
    b: Body<Child>,
) -> Result<Reply> {
    s.child(id(p)?, owner(w, &h)?, key(&h)?, body(b)?).await
}
async fn messages(
    State(s): State<RuntimeService>,
    Extension(w): Extension<Uuid>,
    p: Id,
    h: HeaderMap,
    b: Body<RunMessage>,
) -> Result<Reply> {
    s.run_message(id(p)?, owner(w, &h)?, key(&h)?, body(b)?)
        .await
}
async fn follow_up(
    State(s): State<RuntimeService>,
    Extension(w): Extension<Uuid>,
    p: Id,
    h: HeaderMap,
    b: Body<FollowUp>,
) -> Result<Reply> {
    s.follow_up(id(p)?, owner(w, &h)?, key(&h)?, body(b)?).await
}
async fn abort(
    State(s): State<RuntimeService>,
    Extension(w): Extension<Uuid>,
    p: Id,
    h: HeaderMap,
    b: Body<AbortRequest>,
) -> Result<Reply> {
    s.abort_request(id(p)?, owner(w, &h)?, key(&h)?, body(b)?)
        .await
}
