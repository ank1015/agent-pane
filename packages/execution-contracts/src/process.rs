use std::collections::BTreeMap;

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::{
    ArtifactId, Base64Data, ExecutionId, OperationId, PathSpec, TimestampMs, Validate,
    ValidationError,
    validation::{append_nested, finish, issue, require_non_empty},
};

#[derive(Clone, Debug, Deserialize, Eq, JsonSchema, PartialEq, Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum CommandSpec {
    Shell {
        command: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        shell: Option<String>,
        #[serde(default)]
        login: bool,
    },
    Argv {
        program: String,
        #[serde(default)]
        arguments: Vec<String>,
    },
}

#[derive(Clone, Debug, Deserialize, Eq, JsonSchema, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum EnvironmentInheritance {
    All,
    None,
    AllowList,
}

#[derive(Clone, Debug, Deserialize, Eq, JsonSchema, PartialEq, Serialize)]
pub struct EnvironmentVariables {
    pub inherit: EnvironmentInheritance,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub allow: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub remove: Vec<String>,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub set: BTreeMap<String, String>,
}

impl Default for EnvironmentVariables {
    fn default() -> Self {
        Self {
            inherit: EnvironmentInheritance::All,
            allow: Vec::new(),
            remove: Vec::new(),
            set: BTreeMap::new(),
        }
    }
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, JsonSchema, PartialEq, Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum StdinMode {
    Closed,
    Pipe,
    Pty { columns: u16, rows: u16 },
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, JsonSchema, PartialEq, Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ExecutionPersistence {
    KillOnDisconnect,
    KeepUntilExit,
    KeepFor { retention_ms: u64 },
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, JsonSchema, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum SandboxMode {
    Required,
    BestEffort,
    Disabled,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, JsonSchema, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum NetworkMode {
    Inherit,
    Denied,
    Allowed,
}

#[derive(Clone, Debug, Deserialize, Eq, JsonSchema, PartialEq, Serialize)]
pub struct ResourceLimits {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub memory_bytes: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cpu_time_ms: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub process_count: Option<u32>,
}

#[derive(Clone, Debug, Deserialize, Eq, JsonSchema, PartialEq, Serialize)]
pub struct ExecutionPolicy {
    pub sandbox: SandboxMode,
    pub network: NetworkMode,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub profile: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub resource_limits: Option<ResourceLimits>,
}

#[derive(Clone, Debug, Deserialize, Eq, JsonSchema, PartialEq, Serialize)]
pub struct ProcessOutputPolicy {
    #[serde(default = "default_true")]
    pub persist_full_output: bool,
    pub max_inline_bytes: u64,
    pub max_chunk_bytes: u32,
}

#[derive(Clone, Debug, Deserialize, Eq, JsonSchema, PartialEq, Serialize)]
pub struct StartExecutionRequest {
    pub operation_id: OperationId,
    pub execution_id: ExecutionId,
    pub command: CommandSpec,
    pub cwd: PathSpec,
    #[serde(default)]
    pub environment: EnvironmentVariables,
    pub stdin: StdinMode,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub timeout_ms: Option<u64>,
    pub persistence: ExecutionPersistence,
    pub policy: ExecutionPolicy,
    pub output: ProcessOutputPolicy,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, JsonSchema, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ExecutionState {
    Queued,
    Starting,
    Running,
    Exited,
    Failed,
    Cancelled,
    Lost,
}

#[derive(Clone, Debug, Deserialize, Eq, JsonSchema, PartialEq, Serialize)]
pub struct ExecutionHandle {
    pub execution_id: ExecutionId,
    pub state: ExecutionState,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub started_at: Option<TimestampMs>,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, JsonSchema, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ProcessOutputStream {
    Stdout,
    Stderr,
    Pty,
}

#[derive(Clone, Debug, Deserialize, Eq, JsonSchema, PartialEq, Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ProcessEventKind {
    Started,
    Output {
        stream: ProcessOutputStream,
        data: Base64Data,
    },
    Exited {
        exit_code: i32,
        #[serde(default)]
        sandbox_denied: bool,
    },
    Closed,
    Failed {
        message: String,
    },
}

#[derive(Clone, Debug, Deserialize, Eq, JsonSchema, PartialEq, Serialize)]
pub struct ProcessEvent {
    pub execution_id: ExecutionId,
    pub sequence: u64,
    pub timestamp: TimestampMs,
    pub event: ProcessEventKind,
}

#[derive(Clone, Debug, Deserialize, Eq, JsonSchema, PartialEq, Serialize)]
pub struct AttachExecutionRequest {
    pub execution_id: ExecutionId,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub after_sequence: Option<u64>,
}

#[derive(Clone, Debug, Deserialize, Eq, JsonSchema, PartialEq, Serialize)]
pub struct InspectExecutionRequest {
    pub execution_id: ExecutionId,
}

#[derive(Clone, Debug, Deserialize, Eq, JsonSchema, PartialEq, Serialize)]
pub struct WriteProcessInputRequest {
    pub execution_id: ExecutionId,
    pub write_id: OperationId,
    pub data: Base64Data,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, JsonSchema, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ProcessInputStatus {
    Accepted,
    UnknownExecution,
    StdinClosed,
    Starting,
}

#[derive(Clone, Debug, Deserialize, Eq, JsonSchema, PartialEq, Serialize)]
pub struct WriteProcessInputResult {
    pub status: ProcessInputStatus,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, JsonSchema, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ProcessSignal {
    Interrupt,
    Terminate,
    Kill,
    Hangup,
    User1,
    User2,
}

#[derive(Clone, Debug, Deserialize, Eq, JsonSchema, PartialEq, Serialize)]
pub struct SignalExecutionRequest {
    pub execution_id: ExecutionId,
    pub signal: ProcessSignal,
}

#[derive(Clone, Debug, Deserialize, Eq, JsonSchema, PartialEq, Serialize)]
pub struct TerminateExecutionRequest {
    pub execution_id: ExecutionId,
}

#[derive(Clone, Debug, Deserialize, Eq, JsonSchema, PartialEq, Serialize)]
pub struct TerminateExecutionResult {
    /// Whether the execution was still active when termination was requested.
    pub was_running: bool,
}

#[derive(Clone, Debug, Deserialize, Eq, JsonSchema, PartialEq, Serialize)]
pub struct ResizePtyRequest {
    pub execution_id: ExecutionId,
    pub columns: u16,
    pub rows: u16,
}

#[derive(Clone, Debug, Deserialize, Eq, JsonSchema, PartialEq, Serialize)]
pub struct ExecutionStatus {
    pub execution_id: ExecutionId,
    pub state: ExecutionState,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub started_at: Option<TimestampMs>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub finished_at: Option<TimestampMs>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub exit_code: Option<i32>,
    pub last_sequence: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub full_output_artifact: Option<ArtifactId>,
}

impl Validate for StartExecutionRequest {
    fn validate(&self) -> Result<(), ValidationError> {
        let mut issues = Vec::new();
        match &self.command {
            CommandSpec::Shell { command, shell, .. } => {
                require_non_empty(&mut issues, "execution.command.command", command);
                if let Some(shell) = shell {
                    require_non_empty(&mut issues, "execution.command.shell", shell);
                }
            }
            CommandSpec::Argv { program, .. } => {
                require_non_empty(&mut issues, "execution.command.program", program);
            }
        }
        append_nested(&mut issues, "execution.cwd", self.cwd.validate());
        if let StdinMode::Pty { columns, rows } = self.stdin
            && (columns == 0 || rows == 0)
        {
            issue(
                &mut issues,
                "execution.stdin",
                "PTY columns and rows must be greater than zero",
            );
        }
        if self.timeout_ms == Some(0) {
            issue(
                &mut issues,
                "execution.timeout_ms",
                "must be greater than zero",
            );
        }
        if let ExecutionPersistence::KeepFor { retention_ms: 0 } = self.persistence {
            issue(
                &mut issues,
                "execution.persistence.retention_ms",
                "must be greater than zero",
            );
        }
        if self.output.max_inline_bytes == 0 {
            issue(
                &mut issues,
                "execution.output.max_inline_bytes",
                "must be greater than zero",
            );
        }
        if self.output.max_chunk_bytes == 0 {
            issue(
                &mut issues,
                "execution.output.max_chunk_bytes",
                "must be greater than zero",
            );
        }
        finish(issues)
    }
}

impl Validate for ResizePtyRequest {
    fn validate(&self) -> Result<(), ValidationError> {
        let mut issues = Vec::new();
        if self.columns == 0 || self.rows == 0 {
            issue(
                &mut issues,
                "resize_pty",
                "columns and rows must be greater than zero",
            );
        }
        finish(issues)
    }
}

const fn default_true() -> bool {
    true
}
