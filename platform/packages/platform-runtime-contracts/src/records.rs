use crate::{Lease, WaitMode, WorkerStatus};
use chrono::{DateTime, Utc};
use llm_contracts::{JsonObject, Message};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RunStatus {
    Ready,
    Running,
    Waiting,
    Completed,
    Failed,
    Aborted,
}

/// Public run state. Ownership is available only in worker responses.
#[derive(Serialize, Deserialize)]
pub struct Run {
    pub id: Uuid,
    pub project_id: Uuid,
    pub session_id: Uuid,
    pub parent_run_id: Option<Uuid>,
    pub config: JsonObject,
    pub status: RunStatus,
    pub version: i64,
    pub available_at: Option<DateTime<Utc>>,
    pub abort_requested_at: Option<DateTime<Utc>>,
    pub final_message_id: Option<Uuid>,
    pub error: Option<JsonObject>,
    pub last_event_sequence: i64,
    pub created_at: DateTime<Utc>,
    pub started_at: Option<DateTime<Utc>>,
    pub finished_at: Option<DateTime<Utc>>,
}

#[derive(Serialize, Deserialize)]
pub struct OwnedRun {
    #[serde(flatten)]
    pub run: Run,
    pub worker_id: Option<Uuid>,
    pub lease_epoch: i64,
    pub lease_expires_at: Option<DateTime<Utc>>,
}

#[derive(Serialize, Deserialize)]
pub struct Assignment {
    #[serde(flatten)]
    pub run: OwnedRun,
    pub harness_id: String,
}
impl Assignment {
    pub fn lease(&self) -> Lease {
        Lease {
            run_id: self.run.run.id,
            lease_epoch: self.run.lease_epoch,
        }
    }
}

#[derive(Serialize, Deserialize)]
pub struct Session {
    pub id: Uuid,
    pub project_id: Uuid,
    pub harness_id: String,
    pub title: Option<String>,
    pub forked_from_session_id: Option<Uuid>,
    pub forked_at_revision: Option<i64>,
    pub current_revision: i64,
    pub created_at: DateTime<Utc>,
    pub last_activity_at: DateTime<Utc>,
    pub archived_at: Option<DateTime<Utc>>,
    pub active_run: Option<Run>,
}

#[derive(Serialize, Deserialize)]
pub struct Worker {
    pub id: Uuid,
    pub build_id: String,
    pub supported_harnesses: Vec<String>,
    pub status: WorkerStatus,
    pub capacity: i32,
    pub started_at: DateTime<Utc>,
    pub last_seen_at: DateTime<Utc>,
}
#[derive(Serialize, Deserialize)]
pub struct ClaimResponse {
    pub items: Vec<Assignment>,
    pub lease_duration_seconds: u32,
}
#[derive(Serialize, Deserialize)]
pub struct AssignmentsResponse {
    pub worker: Worker,
    pub items: Vec<Assignment>,
}
#[derive(Serialize, Deserialize)]
pub struct RenewedLease {
    pub run_id: Uuid,
    pub lease_epoch: i64,
    pub lease_expires_at: DateTime<Utc>,
    pub abort_requested_at: Option<DateTime<Utc>>,
    /// Observational only: not a replacement for versions read with context.
    pub version: i64,
}
#[derive(Serialize, Deserialize)]
pub struct HeartbeatResponse {
    pub renewed: Vec<RenewedLease>,
    pub lost: Vec<Lease>,
    pub lease_duration_seconds: u32,
}
#[derive(Serialize, Deserialize)]
pub struct SavedCheckpoint {
    pub run_id: Uuid,
    pub version: i64,
    pub state: JsonObject,
    pub saved_by_lease_epoch: i64,
    pub updated_at: DateTime<Utc>,
}
#[derive(Serialize, Deserialize)]
pub struct SessionMessage {
    pub message_id: Uuid,
    pub revision: i64,
    pub run_id: Option<Uuid>,
    pub origin_run_id: Option<Uuid>,
    pub message: Message,
    pub created_at: DateTime<Utc>,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum InputState {
    Pending,
    Handled,
    Rejected,
}
#[derive(Serialize, Deserialize)]
pub struct RunInput {
    pub id: Uuid,
    pub project_id: Uuid,
    pub run_id: Uuid,
    pub sequence: i64,
    /// Harness-defined kinds remain open strings.
    pub kind: String,
    pub source_run_id: Option<Uuid>,
    pub payload: JsonObject,
    pub status: InputState,
    pub handling: Option<JsonObject>,
    pub created_at: DateTime<Utc>,
    pub handled_at: Option<DateTime<Utc>>,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum WaitStatus {
    Pending,
    Satisfied,
    Cancelled,
    TimedOut,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DependencyKind {
    RunCompletion,
    Input,
    Timer,
    Operation,
}
#[derive(Serialize, Deserialize)]
pub struct WaitDependency {
    pub id: Uuid,
    pub project_id: Uuid,
    pub wait_id: Uuid,
    pub kind: DependencyKind,
    pub target_run_id: Option<Uuid>,
    pub input_kind: Option<String>,
    pub correlation_key: Option<String>,
    pub wake_at: Option<DateTime<Utc>>,
    pub satisfied_at: Option<DateTime<Utc>>,
    pub result: Option<JsonObject>,
}
#[derive(Serialize, Deserialize)]
pub struct RunWait {
    pub id: Uuid,
    pub project_id: Uuid,
    pub run_id: Uuid,
    pub wait_key: String,
    pub mode: WaitMode,
    pub status: WaitStatus,
    pub deadline_at: Option<DateTime<Utc>>,
    pub metadata: JsonObject,
    pub result: Option<JsonObject>,
    pub created_at: DateTime<Utc>,
    pub resolved_at: Option<DateTime<Utc>>,
    pub dependencies: Vec<WaitDependency>,
}
#[derive(Serialize, Deserialize)]
pub struct RunEvent {
    pub id: Uuid,
    pub run_id: Uuid,
    pub sequence: i64,
    pub r#type: String,
    pub source: String,
    pub payload: JsonObject,
    pub occurred_at: DateTime<Utc>,
    pub recorded_at: DateTime<Utc>,
}
#[derive(Serialize, Deserialize)]
pub struct Items<T> {
    pub items: Vec<T>,
}
#[derive(Serialize, Deserialize)]
pub struct CursorPage<T> {
    pub items: Vec<T>,
    pub next_cursor: Option<String>,
}
#[derive(Serialize, Deserialize)]
pub struct SequencePage<T> {
    pub items: Vec<T>,
    pub next_after_sequence: Option<i64>,
}
#[derive(Serialize, Deserialize)]
pub struct MessagePage {
    pub items: Vec<SessionMessage>,
    pub next_after_revision: Option<i64>,
}
#[derive(Serialize, Deserialize)]
pub struct ContextWaitPage {
    pub items: Vec<RunWait>,
    pub next_after_wait_id: Option<Uuid>,
}
#[derive(Serialize, Deserialize)]
pub struct RunContext {
    pub run: OwnedRun,
    pub session: Session,
    pub checkpoint: Option<SavedCheckpoint>,
    pub messages: MessagePage,
    pub waits: ContextWaitPage,
}
#[derive(Serialize, Deserialize)]
pub struct CommitResponse {
    pub run: OwnedRun,
    pub session_revision: i64,
    pub checkpoint: Option<SavedCheckpoint>,
    pub message_ids: Vec<Uuid>,
    pub wait_ids: Vec<Uuid>,
    pub events: Vec<RunEvent>,
}
#[derive(Serialize, Deserialize)]
pub struct ChildResponse {
    pub session: Session,
    pub run: Run,
    pub input: RunInput,
}
#[derive(Serialize, Deserialize)]
pub struct MessageResponse {
    pub run: Run,
    pub input: RunInput,
}
#[derive(Serialize, Deserialize)]
pub struct AbortResponse {
    pub run: Run,
    pub input: Option<RunInput>,
}
#[derive(Serialize, Deserialize)]
pub struct Harness {
    pub id: String,
    pub name: String,
    pub description: Option<String>,
    pub default_config: JsonObject,
    pub config_schema: Option<JsonObject>,
    pub enabled: bool,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SequenceQuery {
    pub limit: Option<u32>,
    pub after_sequence: Option<i64>,
    pub status: Option<String>,
}
#[derive(Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MessageQuery {
    pub limit: Option<u32>,
    pub after_revision: Option<i64>,
    pub run_id: Option<Uuid>,
}
#[derive(Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ListQuery {
    pub limit: Option<u32>,
    pub cursor: Option<String>,
    pub status: Option<String>,
    pub archived: Option<bool>,
}
