//! Typed worker transport, not a worker scheduler or harness loop.
//!
//! Persist [`Command`] values for recovery-critical effects. A [`RunClient`]
//! binds a lease epoch, but never claims that the lease is still valid and never
//! updates optimistic versions for the caller. See the package README.
#![doc = include_str!("../README.md")]
mod error;
mod identity;
mod queries;
mod transport;
pub use error::{Error, Result, ServerError};
pub use identity::{Command, RequestKey};
pub use platform_runtime_contracts as types;
pub use queries::QueryClient;
pub use transport::ClientConfig;

use reqwest::{Method, RequestBuilder, header::HeaderValue};
use serde::{Serialize, de::DeserializeOwned};
use std::sync::Arc;
use transport::Transport;
use types::*;
use uuid::Uuid;

/// Non-secret registration metadata. Worker identity and token are configured
/// once on the client; registration cannot accidentally send a different token.
pub struct WorkerRegistration {
    pub build_id: String,
    pub supported_harnesses: Vec<String>,
    pub capacity: i32,
}
struct Inner {
    transport: Transport,
    worker_id: Uuid,
    token: String,
}
/// Cheap to clone; shares a connection pool and a single worker identity.
#[derive(Clone)]
pub struct PlatformClient {
    inner: Arc<Inner>,
}
impl std::fmt::Debug for PlatformClient {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("PlatformClient")
            .field("worker_id", &self.inner.worker_id)
            .finish_non_exhaustive()
    }
}
fn bearer(token: &str) -> Result<HeaderValue> {
    if !(32..=256).contains(&token.len()) || !token.bytes().all(|b| (33..=126).contains(&b)) {
        return Err(Error::Invalid(
            "tokens require 32–256 printable ASCII characters without spaces",
        ));
    }
    let mut header = HeaderValue::from_str(&format!("Bearer {token}"))
        .map_err(|_| Error::Invalid("invalid token"))?;
    header.set_sensitive(true);
    Ok(header)
}
impl PlatformClient {
    pub fn new(
        base_url: &str,
        worker_id: Uuid,
        worker_token: impl Into<String>,
        config: ClientConfig,
    ) -> Result<Self> {
        let token = worker_token.into();
        bearer(&token)?;
        Ok(Self {
            inner: Arc::new(Inner {
                transport: Transport::new(base_url, config)?,
                worker_id,
                token,
            }),
        })
    }
    pub fn worker_id(&self) -> Uuid {
        self.inner.worker_id
    }
    /// Read-only view sharing this client's connection pool and transport policy.
    pub fn queries(&self) -> QueryClient {
        QueryClient::new(self.clone())
    }
    fn request(&self, method: Method, path: &str) -> Result<RequestBuilder> {
        Ok(self
            .inner
            .transport
            .request(method, path)?
            .header("authorization", bearer(&self.inner.token)?))
    }
    fn worker_path(&self, suffix: &str) -> String {
        format!("internal/workers/{}{suffix}", self.worker_id())
    }

    /// Fresh UUID/token for each worker process. Bootstrap is used only here,
    /// not retained by the client, and never used on subsequent calls.
    pub async fn register(
        &self,
        bootstrap_token: &str,
        registration: &WorkerRegistration,
    ) -> Result<Worker> {
        let body = RegisterWorker {
            build_id: registration.build_id.clone(),
            supported_harnesses: registration.supported_harnesses.clone(),
            capacity: registration.capacity,
            worker_token: self.inner.token.clone(),
        };
        let req = self
            .inner
            .transport
            .request(Method::PUT, &self.worker_path(""))?
            .header("authorization", bearer(bootstrap_token)?)
            .json(&body);
        self.inner.transport.send(req, true).await
    }
    pub async fn heartbeat(&self, request: &Heartbeat) -> Result<HeartbeatResponse> {
        self.inner
            .transport
            .send(
                self.request(Method::POST, &self.worker_path("/heartbeat"))?
                    .json(request),
                true,
            )
            .await
    }
    /// A new allocation attempt needs a NEW command key, even after an empty
    /// result. Replays return historical leases: reconcile before executing.
    pub async fn claim(&self, command: &Command<Claim>) -> Result<ClaimResponse> {
        let req = self
            .request(Method::POST, &self.worker_path("/claims"))?
            .header("idempotency-key", command.key().as_str())
            .json(command.body());
        self.inner.transport.send(req, true).await
    }
    pub async fn assignments(&self) -> Result<AssignmentsResponse> {
        self.inner
            .transport
            .send(
                self.request(Method::GET, &self.worker_path("/assignments"))?,
                true,
            )
            .await
    }
    /// No automatic retry: PATCH has no receipt, and replay after a concurrent
    /// lifecycle change could overwrite newer administrative intent.
    pub async fn patch_worker(&self, request: &PatchWorker) -> Result<Worker> {
        self.inner
            .transport
            .send(
                self.request(Method::PATCH, &self.worker_path(""))?
                    .json(request),
                false,
            )
            .await
    }
    /// Also accepts a historical lease for receipt recovery. This is not proof
    /// of current ownership; the server fences every new read/write.
    pub fn run(&self, lease: Lease) -> Result<RunClient> {
        if lease.lease_epoch < 1 {
            return Err(Error::Invalid("lease epoch must be positive"));
        }
        Ok(RunClient {
            client: self.clone(),
            lease,
        })
    }

    async fn get<T: DeserializeOwned, Q: Serialize>(&self, path: &str, query: &Q) -> Result<T> {
        self.inner
            .transport
            .send(self.request(Method::GET, path)?.query(query), true)
            .await
    }
    pub async fn harnesses(&self) -> Result<Items<Harness>> {
        self.get("api/harnesses", &()).await
    }
    pub async fn harness(&self, id: &str) -> Result<Harness> {
        // The server's harness ID grammar; avoids arbitrary path traversal.
        if !is_valid_harness_id(id) {
            return Err(Error::Invalid("invalid harness ID"));
        }
        self.get(&format!("api/harnesses/{id}"), &()).await
    }
    pub async fn session(&self, id: Uuid) -> Result<Session> {
        self.get(&format!("api/sessions/{id}"), &()).await
    }
    pub async fn sessions(&self, project: Uuid, query: &ListQuery) -> Result<CursorPage<Session>> {
        self.get(&format!("api/projects/{project}/sessions"), query)
            .await
    }
    pub async fn session_messages(&self, id: Uuid, query: &MessageQuery) -> Result<MessagePage> {
        self.get(&format!("api/sessions/{id}/messages"), query)
            .await
    }
    pub async fn session_runs(&self, id: Uuid, query: &ListQuery) -> Result<CursorPage<Run>> {
        self.get(&format!("api/sessions/{id}/runs"), query).await
    }
    pub async fn run_state(&self, id: Uuid) -> Result<Run> {
        self.get(&format!("api/runs/{id}"), &()).await
    }
    pub async fn children(&self, id: Uuid, query: &ListQuery) -> Result<CursorPage<Run>> {
        self.get(&format!("api/runs/{id}/children"), query).await
    }
    pub async fn run_waits(&self, id: Uuid, query: &ListQuery) -> Result<CursorPage<RunWait>> {
        self.get(&format!("api/runs/{id}/waits"), query).await
    }
    /// Durable replay pages, suitable for recovery/polling. No implicit SSE
    /// subscription or automatic acknowledgement of the returned cursor.
    pub async fn run_events(
        &self,
        id: Uuid,
        query: &SequenceQuery,
    ) -> Result<SequencePage<RunEvent>> {
        self.get(&format!("api/runs/{id}/events"), query).await
    }
    pub async fn run_inputs(
        &self,
        id: Uuid,
        query: &SequenceQuery,
    ) -> Result<SequencePage<RunInput>> {
        self.get(&format!("api/runs/{id}/inputs"), query).await
    }
}

#[derive(Clone)]
pub struct RunClient {
    client: PlatformClient,
    lease: Lease,
}
impl RunClient {
    /// Shared history/coordination reads, including other runs and sessions.
    /// These observations do not confer ownership or bypass fenced mutations.
    pub fn queries(&self) -> QueryClient {
        self.client.queries()
    }
    pub fn lease(&self) -> Lease {
        self.lease
    }
    fn request(&self, method: Method, suffix: &str) -> Result<RequestBuilder> {
        Ok(self
            .client
            .request(
                method,
                &format!("internal/runs/{}{suffix}", self.lease.run_id),
            )?
            .header("x-worker-id", self.client.worker_id().to_string())
            .header("x-lease-epoch", self.lease.lease_epoch))
    }
    pub async fn context(&self, query: &ContextQuery) -> Result<RunContext> {
        self.client
            .inner
            .transport
            .send(self.request(Method::GET, "/context")?.query(query), true)
            .await
    }
    /// Defaults to pending. Reading does not acknowledge inputs. Re-scan from
    /// sequence zero after recovery; acknowledge only in an atomic commit.
    pub async fn inputs(&self, query: &SequenceQuery) -> Result<SequencePage<RunInput>> {
        self.client
            .inner
            .transport
            .send(self.request(Method::GET, "/inputs")?.query(query), true)
            .await
    }
    async fn post<Q: Serialize, T: DeserializeOwned>(
        &self,
        suffix: &str,
        command: &Command<Q>,
    ) -> Result<T> {
        let req = self
            .request(Method::POST, suffix)?
            .header("idempotency-key", command.key().as_str())
            .json(command.body());
        self.client.inner.transport.send(req, true).await
    }
    pub async fn commit(&self, command: &Command<Commit>) -> Result<CommitResponse> {
        self.post("/commits", command).await
    }
    pub async fn append_events(&self, command: &Command<Events>) -> Result<Items<RunEvent>> {
        self.post("/events", command).await
    }
    pub async fn create_child(&self, command: &Command<Child>) -> Result<ChildResponse> {
        self.post("/children", command).await
    }
    /// Start a new run in an existing idle session, preserving its history.
    /// For a live run, use `send_message` instead.
    pub async fn follow_up(&self, command: &Command<FollowUp>) -> Result<ChildResponse> {
        self.post("/follow-ups", command).await
    }
    pub async fn send_message(&self, command: &Command<RunMessage>) -> Result<MessageResponse> {
        self.post("/messages", command).await
    }
    pub async fn request_abort(&self, command: &Command<AbortRequest>) -> Result<AbortResponse> {
        self.post("/abort-requests", command).await
    }
}
