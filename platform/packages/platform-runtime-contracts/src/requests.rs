use chrono::{DateTime, Utc};
use llm_contracts::{JsonObject, Message};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

// This credential is serialized only for registration; deliberately no Debug.
#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct RegisterWorker {
    pub build_id: String,
    pub supported_harnesses: Vec<String>,
    pub capacity: i32,
    pub worker_token: String,
}
#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct PatchWorker {
    pub status: Option<WorkerStatus>,
    pub capacity: Option<i32>,
}
#[derive(Deserialize, Serialize, Clone, Copy)]
#[serde(rename_all = "snake_case")]
pub enum WorkerStatus {
    Accepting,
    Draining,
    Offline,
}
impl WorkerStatus {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Accepting => "accepting",
            Self::Draining => "draining",
            Self::Offline => "offline",
        }
    }
}
#[derive(Deserialize, Serialize, Clone, Copy)]
#[serde(deny_unknown_fields)]
pub struct Lease {
    pub run_id: Uuid,
    pub lease_epoch: i64,
}
#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Heartbeat {
    #[serde(default)]
    pub leases: Vec<Lease>,
}
#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Claim {
    pub limit: u32,
}

#[derive(Deserialize, Serialize, Default)]
#[serde(deny_unknown_fields)]
pub struct ContextQuery {
    pub after_revision: Option<i64>,
    pub limit: Option<u32>,
    pub after_wait_id: Option<Uuid>,
}
#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct HarnessEvent {
    pub r#type: String,
    #[serde(default)]
    pub payload: JsonObject,
    pub occurred_at: Option<DateTime<Utc>>,
}
#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Events {
    pub events: Vec<HarnessEvent>,
}
#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Checkpoint {
    pub expected_version: i64,
    pub state: JsonObject,
}
#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct InputResult {
    pub id: Uuid,
    pub status: InputStatus,
    #[serde(default)]
    pub handling: JsonObject,
}
#[derive(Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum InputStatus {
    Handled,
    Rejected,
}
#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Wait {
    pub wait_key: String,
    pub mode: WaitMode,
    pub deadline_at: Option<DateTime<Utc>>,
    #[serde(default)]
    pub metadata: JsonObject,
    pub dependencies: Vec<Dependency>,
}
#[derive(Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum WaitMode {
    Any,
    All,
}
#[derive(Deserialize, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum Dependency {
    RunCompletion {
        target_run_id: Uuid,
    },
    Input {
        input_kind: String,
        correlation_key: Option<String>,
    },
    Timer {
        wake_at: DateTime<Utc>,
    },
    Operation {
        correlation_key: String,
    },
}
#[derive(Deserialize, Serialize, Default)]
#[serde(tag = "status", rename_all = "snake_case", deny_unknown_fields)]
pub enum Disposition {
    #[default]
    Running,
    Ready {
        available_at: Option<DateTime<Utc>>,
    },
    Waiting,
    Completed {
        final_message_id: Uuid,
    },
    Failed {
        error: JsonObject,
    },
    Aborted,
}
#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct AppendMessage {
    pub message_id: Uuid,
    pub message: Message,
}
#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Commit {
    pub expected_run_version: i64,
    pub expected_session_revision: i64,
    pub checkpoint: Option<Checkpoint>,
    #[serde(default)]
    pub messages: Vec<AppendMessage>,
    #[serde(default)]
    pub input_results: Vec<InputResult>,
    #[serde(default)]
    pub events: Vec<HarnessEvent>,
    #[serde(default)]
    pub waits: Vec<Wait>,
    #[serde(default)]
    pub cancel_wait_ids: Vec<Uuid>,
    #[serde(default)]
    pub disposition: Disposition,
}
impl Commit {
    /// Start an otherwise empty commit from versions read together in context.
    pub fn new(expected_run_version: i64, expected_session_revision: i64) -> Self {
        Self {
            expected_run_version,
            expected_session_revision,
            checkpoint: None,
            messages: Vec::new(),
            input_results: Vec::new(),
            events: Vec::new(),
            waits: Vec::new(),
            cancel_wait_ids: Vec::new(),
            disposition: Disposition::Running,
        }
    }
}
#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Child {
    pub harness_id: Option<String>,
    pub title: Option<String>,
    /// None creates an empty child session; Some copies this run's session prefix.
    pub fork_at_revision: Option<i64>,
    pub initial_run: StartRun,
}
#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct RunMessage {
    pub target_run_id: Uuid,
    pub kind: String,
    #[serde(default)]
    pub payload: JsonObject,
}
/// Start another run without creating or copying the target session's history.
#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct FollowUp {
    pub target_session_id: Uuid,
    pub run: StartRun,
}
#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct AbortRequest {
    pub target_run_id: Uuid,
    pub reason: Option<String>,
}

#[derive(Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct StartRun {
    pub input: Message,
    #[serde(default)]
    pub config_override: JsonObject,
    pub expected_session_revision: i64,
}
