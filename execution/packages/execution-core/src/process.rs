use std::collections::{BTreeMap, BTreeSet};

use async_trait::async_trait;
use serde::{Deserialize, Serialize};

use crate::{
    BinaryData, ExecutionId, ExecutionPath, ExecutionResult, OperationContext, OperationId,
    SupervisorGenerationId, TimestampMs, Validate, ValidationError, WriteId,
    validation::{append_nested, finish, issue, require_non_empty},
};

/// A command without implicit shell interpretation unless explicitly requested.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
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

impl Validate for CommandSpec {
    fn validate(&self) -> Result<(), ValidationError> {
        let mut issues = Vec::new();
        match self {
            Self::Shell { command, shell, .. } => {
                require_non_empty(&mut issues, "command", command);
                if command.contains('\0') {
                    issue(&mut issues, "command", "must not contain a null byte");
                }
                if let Some(shell) = shell {
                    require_non_empty(&mut issues, "shell", shell);
                    if shell.contains('\0') {
                        issue(&mut issues, "shell", "must not contain a null byte");
                    }
                }
            }
            Self::Argv { program, arguments } => {
                require_non_empty(&mut issues, "program", program);
                if arguments.iter().any(|value| value.contains('\0')) {
                    issue(&mut issues, "arguments", "must not contain a null byte");
                }
            }
        }
        finish(issues)
    }
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum EnvironmentMode {
    Inherit,
    Empty,
}

/// Environment construction for a child process.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct EnvironmentVariables {
    pub mode: EnvironmentMode,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub set: BTreeMap<String, String>,
    #[serde(default, skip_serializing_if = "BTreeSet::is_empty")]
    pub remove: BTreeSet<String>,
}

impl Default for EnvironmentVariables {
    fn default() -> Self {
        Self {
            mode: EnvironmentMode::Inherit,
            set: BTreeMap::new(),
            remove: BTreeSet::new(),
        }
    }
}

impl Validate for EnvironmentVariables {
    fn validate(&self) -> Result<(), ValidationError> {
        let mut issues = Vec::new();
        for (name, value) in &self.set {
            validate_environment_name(&mut issues, "set", name);
            if value.contains('\0') {
                issue(
                    &mut issues,
                    format!("set.{name}"),
                    "must not contain a null byte",
                );
            }
        }
        for name in &self.remove {
            validate_environment_name(&mut issues, "remove", name);
            if self.set.contains_key(name) {
                issue(
                    &mut issues,
                    format!("remove.{name}"),
                    "must not also appear in set",
                );
            }
        }
        finish(issues)
    }
}

fn validate_environment_name(issues: &mut Vec<crate::ValidationIssue>, field: &str, name: &str) {
    if name.is_empty() {
        issue(issues, field, "variable names must not be empty");
    } else if name.contains(['=', '\0']) {
        issue(
            issues,
            format!("{field}.{name}"),
            "variable names must not contain '=' or a null byte",
        );
    }
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum StdinMode {
    Closed,
    Pipe,
    Pty { columns: u16, rows: u16 },
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct StartExecutionRequest {
    pub operation_id: OperationId,
    pub execution_id: ExecutionId,
    pub command: CommandSpec,
    pub cwd: ExecutionPath,
    #[serde(default)]
    pub environment: EnvironmentVariables,
    pub stdin: StdinMode,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub timeout_ms: Option<u64>,
}

impl Validate for StartExecutionRequest {
    fn validate(&self) -> Result<(), ValidationError> {
        let mut issues = Vec::new();
        append_nested(&mut issues, "command", self.command.validate());
        append_nested(&mut issues, "cwd", self.cwd.validate());
        append_nested(&mut issues, "environment", self.environment.validate());
        if let StdinMode::Pty { columns, rows } = self.stdin {
            if columns == 0 || rows == 0 {
                issue(
                    &mut issues,
                    "stdin",
                    "PTY columns and rows must be greater than zero",
                );
            }
        }
        if self.timeout_ms == Some(0) {
            issue(&mut issues, "timeout_ms", "must be greater than zero");
        }
        finish(issues)
    }
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ExecutionState {
    Starting,
    Running,
    Exited,
    Failed,
    Cancelled,
    Lost,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ExecutionHandle {
    pub execution_id: ExecutionId,
    pub supervisor_generation_id: SupervisorGenerationId,
    pub state: ExecutionState,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub started_at: Option<TimestampMs>,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ProcessOutputStream {
    Stdout,
    Stderr,
    Pty,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ProcessEventKind {
    Started,
    Output {
        stream: ProcessOutputStream,
        data: BinaryData,
    },
    Exited {
        exit_code: i32,
    },
    Failed {
        message: String,
    },
    Closed,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ProcessEvent {
    pub sequence: u64,
    pub timestamp: TimestampMs,
    pub event: ProcessEventKind,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ReadExecutionRequest {
    pub execution_id: ExecutionId,
    pub supervisor_generation_id: SupervisorGenerationId,
    #[serde(default)]
    pub after_sequence: u64,
    pub max_bytes: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub wait_ms: Option<u64>,
}

impl Validate for ReadExecutionRequest {
    fn validate(&self) -> Result<(), ValidationError> {
        let mut issues = Vec::new();
        if self.max_bytes == 0 {
            issue(&mut issues, "max_bytes", "must be greater than zero");
        }
        if self.wait_ms == Some(0) {
            issue(&mut issues, "wait_ms", "must be greater than zero when set");
        }
        finish(issues)
    }
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ReadExecutionResult {
    pub events: Vec<ProcessEvent>,
    /// Highest sequence included in `events`, or the supplied cursor when empty.
    pub next_sequence: u64,
    pub state: ExecutionState,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub exit_code: Option<i32>,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ProcessInput {
    Data { data: BinaryData },
    Close,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct WriteProcessInputRequest {
    pub execution_id: ExecutionId,
    pub supervisor_generation_id: SupervisorGenerationId,
    pub write_id: WriteId,
    pub input: ProcessInput,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ProcessInputStatus {
    Accepted,
    AlreadyAccepted,
    Starting,
    StdinClosed,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct WriteProcessInputResult {
    pub status: ProcessInputStatus,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ResizePtyRequest {
    pub operation_id: OperationId,
    pub execution_id: ExecutionId,
    pub supervisor_generation_id: SupervisorGenerationId,
    pub columns: u16,
    pub rows: u16,
}

impl Validate for ResizePtyRequest {
    fn validate(&self) -> Result<(), ValidationError> {
        if self.columns == 0 || self.rows == 0 {
            Err(ValidationError::single(
                "pty",
                "columns and rows must be greater than zero",
            ))
        } else {
            Ok(())
        }
    }
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ProcessSignal {
    Interrupt,
    Terminate,
    Kill,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct SignalExecutionRequest {
    pub operation_id: OperationId,
    pub execution_id: ExecutionId,
    pub supervisor_generation_id: SupervisorGenerationId,
    pub signal: ProcessSignal,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct TerminateExecutionRequest {
    pub operation_id: OperationId,
    pub execution_id: ExecutionId,
    pub supervisor_generation_id: SupervisorGenerationId,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct TerminateExecutionResult {
    pub was_running: bool,
}

#[async_trait]
pub trait ProcessRuntime: Send + Sync {
    async fn start(
        &self,
        context: &OperationContext,
        request: StartExecutionRequest,
    ) -> ExecutionResult<ExecutionHandle>;

    async fn read(
        &self,
        context: &OperationContext,
        request: ReadExecutionRequest,
    ) -> ExecutionResult<ReadExecutionResult>;

    async fn write(
        &self,
        context: &OperationContext,
        request: WriteProcessInputRequest,
    ) -> ExecutionResult<WriteProcessInputResult>;

    async fn resize(
        &self,
        context: &OperationContext,
        request: ResizePtyRequest,
    ) -> ExecutionResult<()>;

    async fn signal(
        &self,
        context: &OperationContext,
        request: SignalExecutionRequest,
    ) -> ExecutionResult<()>;

    async fn terminate(
        &self,
        context: &OperationContext,
        request: TerminateExecutionRequest,
    ) -> ExecutionResult<TerminateExecutionResult>;
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::RootId;

    fn request() -> StartExecutionRequest {
        StartExecutionRequest {
            operation_id: OperationId::generate(),
            execution_id: ExecutionId::generate(),
            command: CommandSpec::Argv {
                program: "echo".to_string(),
                arguments: vec!["hello".to_string()],
            },
            cwd: ExecutionPath::root(RootId::new("workspace").expect("valid root ID")),
            environment: EnvironmentVariables::default(),
            stdin: StdinMode::Closed,
            timeout_ms: None,
        }
    }

    #[test]
    fn valid_start_request_passes_validation() {
        request().validate().expect("request should be valid");
    }

    #[test]
    fn start_request_rejects_invalid_pty_dimensions() {
        let mut request = request();
        request.stdin = StdinMode::Pty {
            columns: 0,
            rows: 24,
        };
        request.validate().expect_err("zero columns should fail");
    }

    #[test]
    fn environment_rejects_set_and_remove_overlap() {
        let mut environment = EnvironmentVariables::default();
        environment
            .set
            .insert("PATH".to_string(), "/bin".to_string());
        environment.remove.insert("PATH".to_string());
        environment
            .validate()
            .expect_err("overlapping variable should fail");
    }

    #[test]
    fn process_reads_are_bounded() {
        let request = ReadExecutionRequest {
            execution_id: ExecutionId::generate(),
            supervisor_generation_id: SupervisorGenerationId::generate(),
            after_sequence: 0,
            max_bytes: 0,
            wait_ms: None,
        };
        request.validate().expect_err("zero limit should fail");
    }

    #[test]
    fn process_input_serializes_close_explicitly() {
        let value = ProcessInput::Close;
        let json = serde_json::to_value(value).expect("serialize input");
        assert_eq!(json["type"], "close");
    }
}
