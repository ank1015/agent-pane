use crate::{PlatformClient, Result, types::*};
use uuid::Uuid;

/// Read-only access to shared history and coordination state, obtained through
/// [`crate::RunClient::queries`] or [`PlatformClient::queries`].
///
/// Deliberately exposes no registration, claiming, lease construction, or write
/// operations. This is an interface restriction for trusted harnesses, not an
/// authorization mechanism. Reads use the same transport and explicit pagination
/// as the existing Platform client; they do not acknowledge inputs or prove
/// current ownership. No background polling or implicit caching is introduced.
///
/// ```compile_fail
/// # async fn example(reads: platform_runtime_client::QueryClient,
/// # command: platform_runtime_client::Command<platform_runtime_client::types::Claim>) {
/// reads.claim(&command).await; // Worker management is not exposed.
/// # }
/// ```
#[derive(Clone, Debug)]
pub struct QueryClient {
    client: PlatformClient,
}

impl QueryClient {
    pub(crate) fn new(client: PlatformClient) -> Self {
        Self { client }
    }
    pub async fn harnesses(&self) -> Result<Items<Harness>> {
        self.client.harnesses().await
    }
    pub async fn harness(&self, id: &str) -> Result<Harness> {
        self.client.harness(id).await
    }
    pub async fn session(&self, id: Uuid) -> Result<Session> {
        self.client.session(id).await
    }
    pub async fn sessions(&self, project: Uuid, query: &ListQuery) -> Result<CursorPage<Session>> {
        self.client.sessions(project, query).await
    }
    pub async fn session_messages(&self, id: Uuid, query: &MessageQuery) -> Result<MessagePage> {
        self.client.session_messages(id, query).await
    }
    pub async fn session_runs(&self, id: Uuid, query: &ListQuery) -> Result<CursorPage<Run>> {
        self.client.session_runs(id, query).await
    }
    pub async fn run_state(&self, id: Uuid) -> Result<Run> {
        self.client.run_state(id).await
    }
    pub async fn children(&self, id: Uuid, query: &ListQuery) -> Result<CursorPage<Run>> {
        self.client.children(id, query).await
    }
    pub async fn run_waits(&self, id: Uuid, query: &ListQuery) -> Result<CursorPage<RunWait>> {
        self.client.run_waits(id, query).await
    }
    pub async fn run_events(
        &self,
        id: Uuid,
        query: &SequenceQuery,
    ) -> Result<SequencePage<RunEvent>> {
        self.client.run_events(id, query).await
    }
    pub async fn run_inputs(
        &self,
        id: Uuid,
        query: &SequenceQuery,
    ) -> Result<SequencePage<RunInput>> {
        self.client.run_inputs(id, query).await
    }
}
