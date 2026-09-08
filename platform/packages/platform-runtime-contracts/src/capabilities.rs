//! Version 1 shared agent/backend capability vocabulary. These are contracts,
//! not a declaration that every endpoint is deployed. See CAPABILITIES.md.
//! Arguments use camelCase; existing Platform records retain snake_case.
use crate::{JsonObject, Message};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

pub use crate::{
    ArtifactReference, CursorPage, Environment, ExecutionWorkspace, Harness, HarnessContract,
    MessagePage, Run, RunOutput, RunOutputsPage, Session,
};

pub const SDK_VERSION: &str = "1";

/// Authenticated bridge envelope. Scope and credentials belong to the transport.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct CapabilityRequest {
    pub method: String,
    pub args: serde_json::Value,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct MutationOptions {
    pub idempotency_key: String,
}

#[derive(Debug, Default, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct PageOptions {
    pub limit: Option<u32>,
    pub cursor: Option<String>,
}

#[derive(Debug, Default, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct MessageOptions {
    pub limit: Option<u32>,
    pub after_revision: Option<i64>,
    pub run_id: Option<Uuid>,
}

#[derive(Debug, Default, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct OutputOptions {
    pub limit: Option<u32>,
    pub after_sequence: Option<i64>,
}
impl From<OutputOptions> for crate::RunOutputsQuery {
    fn from(value: OutputOptions) -> Self {
        Self {
            limit: value.limit,
            after_sequence: value.after_sequence,
        }
    }
}

/// Credential-free account discovery. Status is open for provider evolution.
#[derive(Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct Account {
    pub account_id: Uuid,
    pub name: String,
    pub provider: String,
    pub status: String,
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct AccountModels {
    pub account_id: Uuid,
    pub name: String,
    pub provider: String,
    pub model_ids: Vec<String>,
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct StartOptions {
    pub harness_id: String,
    pub accounts: Vec<AccountModels>,
    pub config_schema: Option<JsonObject>,
    pub default_config: JsonObject,
    pub harness_contract: HarnessContract,
    /// None for trusted agents; Sites grants explicitly allow top-level fields.
    pub configurable_fields: Option<Vec<String>>,
    pub environment_mode: Option<String>,
}

/// The caller chooses fields declared by this harness. Config includes model
/// and zero/one/multiple environment inputs; accountId is canonical, never a secret.
#[derive(Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct CreateSession {
    pub harness_id: String,
    pub account_id: Uuid,
    pub title: Option<String>,
    pub config: JsonObject,
    #[cfg_attr(
        feature = "schema",
        schemars(with = "Option<crate::schema::UserInput>")
    )]
    pub initial_input: Option<Message>,
    pub on_complete: Option<CompletionCallback>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct CompletionCallback {
    pub path: String,
    #[serde(default)]
    pub payload: serde_json::Value,
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct CreateRun {
    pub session_id: Uuid,
    pub expected_revision: i64,
    #[cfg_attr(feature = "schema", schemars(with = "crate::schema::UserInput"))]
    pub input: Message,
    pub on_complete: Option<CompletionCallback>,
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct RegisteredCallback {
    pub subscription_id: Uuid,
}

#[derive(Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct SessionCreated {
    pub session: Session,
    pub run: Option<Run>,
    pub input: Option<crate::RunInput>,
    pub callback: Option<RegisteredCallback>,
}

#[derive(Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct RunCreated {
    pub run: Run,
    pub input: crate::RunInput,
    pub callback: Option<RegisteredCallback>,
}

pub use crate::{AbortResponse as AbortAccepted, MessageResponse as InputAccepted};

#[derive(Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct SteerRun {
    pub run_id: Uuid,
    #[cfg_attr(feature = "schema", schemars(with = "crate::schema::UserInput"))]
    pub input: Message,
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct AbortRun {
    pub run_id: Uuid,
    pub reason: Option<String>,
}

/// Unknown usage stays null. Counts identify how many assistant messages
/// contributed to each metric; cost is recorded usage, not a billing receipt.
#[derive(Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct UsageMetric {
    pub total: Option<f64>,
    pub contributing_messages: u64,
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct Statistics {
    pub session_id: Uuid,
    pub run_id: Option<Uuid>,
    pub assistant_messages: u64,
    pub input_tokens: UsageMetric,
    pub output_tokens: UsageMetric,
    pub cache_read_tokens: UsageMetric,
    pub cache_write_tokens: UsageMetric,
    pub cost_usd: UsageMetric,
    /// Sum of started-run wall durations, including waits; not CPU or queue time.
    pub run_wall_seconds: Option<f64>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct CreateSandboxFromSnapshot {
    pub environment_id: Uuid,
    pub name: Option<String>,
    pub timeout_seconds: Option<u32>,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub enum SandboxStatus {
    Provisioning,
    Ready,
    Failed,
    Terminated,
    Expired,
    Unavailable,
    Terminating,
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct Sandbox {
    pub id: Uuid,
    pub environment_id: Uuid,
    pub status: SandboxStatus,
    pub workspace: Option<ExecutionWorkspace>,
    pub error: Option<crate::ErrorInfo>,
    pub created_at: DateTime<Utc>,
    pub expires_at: Option<DateTime<Utc>>,
    pub termination_requested: bool,
    pub termination_confirmed: bool,
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct Bash {
    pub host_id: Uuid,
    pub command: String,
    pub workdir: String,
    pub timeout_ms: Option<u64>,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub enum ExecutionStatus {
    Pending,
    Running,
    Completed,
    Failed,
    Cancelled,
    Lost,
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct Execution {
    pub id: Uuid,
    pub host_id: Uuid,
    pub status: ExecutionStatus,
    pub exit_code: Option<i32>,
    pub error: Option<crate::ErrorInfo>,
    pub created_at: DateTime<Utc>,
    pub finished_at: Option<DateTime<Utc>>,
    pub cancellation_requested: bool,
    pub cancellation_confirmed: bool,
    pub output_bytes: u64,
    pub truncated: bool,
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct ExecutionOutput {
    pub execution_id: Uuid,
    pub output: String,
    pub next_cursor: Option<String>,
    pub truncated: bool,
    pub complete: bool,
    pub output_bytes: u64,
}

#[derive(Debug, Default, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct ExecutionOutputOptions {
    pub cursor: Option<String>,
    pub limit_bytes: Option<u32>,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct SandboxId {
    pub sandbox_id: Uuid,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct ExecutionId {
    pub execution_id: Uuid,
}
/// Trusted harness binding, checked against project references and gateway metadata.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct BindHarnessWorkspace {
    pub environment_id: Uuid,
    pub host_id: Uuid,
}
