//! Recoverable Codex-compatible command execution through an injected runtime.
#![doc = include_str!("../README.md")]

mod buffer;
mod output;
mod patch;

use execution_core::*;
use llm_contracts::{FunctionTool, ToolArguments, ToolDefinition};
use rand::Rng;
use serde::{Deserialize, Serialize, de::DeserializeOwned};
use serde_json::{Value, json};
use tool_apply_patch::{
    ApplyPatchConfig, ApplyPatchState, ApplyPatchTool, PatchProgress, RunningPatch,
};
use tool_filesystem::{resolve_path_normalized as resolve_path, validate_cwd};

use buffer::HeadTailBuffer;
pub use output::UnifiedExecOutput;
use output::approximate_tokens;

pub const EXEC_COMMAND_NAME: &str = "exec_command";
pub const WRITE_STDIN_NAME: &str = "write_stdin";
pub const EXEC_COMMAND_DESCRIPTION: &str =
    "Runs a command in a PTY, returning output or a session ID for ongoing interaction.";
pub const WRITE_STDIN_DESCRIPTION: &str =
    "Writes characters to an existing unified exec session and returns recent output.";
pub const DEFAULT_EXEC_YIELD_TIME_MS: u64 = 10_000;
pub const DEFAULT_WRITE_YIELD_TIME_MS: u64 = 250;
pub const MIN_YIELD_TIME_MS: u64 = 250;
pub const MAX_YIELD_TIME_MS: u64 = 30_000;
pub const MIN_EMPTY_YIELD_TIME_MS: u64 = 5_000;
pub const DEFAULT_MAX_EMPTY_YIELD_TIME_MS: u64 = 300_000;
pub const DEFAULT_MAX_OUTPUT_TOKENS: usize = 10_000;
pub const DEFAULT_OUTPUT_MAX_BYTES: usize = 1024 * 1024;
pub const SESSION_STATE_SCHEMA_VERSION: u32 = 1;

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct ExecCommandInput {
    pub cmd: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub workdir: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub shell: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub login: Option<bool>,
    #[serde(default)]
    pub tty: bool,
    #[serde(default = "default_exec_yield_time_ms")]
    pub yield_time_ms: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_output_tokens: Option<usize>,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct WriteStdinInput {
    pub session_id: i32,
    #[serde(default)]
    pub chars: String,
    #[serde(default = "default_write_yield_time_ms")]
    pub yield_time_ms: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_output_tokens: Option<usize>,
}

const fn default_exec_yield_time_ms() -> u64 {
    DEFAULT_EXEC_YIELD_TIME_MS
}

const fn default_write_yield_time_ms() -> u64 {
    DEFAULT_WRITE_YIELD_TIME_MS
}

/// Harness policy and host-side execution settings, never model arguments.
#[derive(Clone, Debug)]
pub struct UnifiedExecConfig {
    /// When false, dispatch every script to the shell without patch recognition.
    /// Prepared commands already pin the resulting shell or patch execution path.
    pub intercept_apply_patch: bool,
    /// Policy for intercepted apply_patch heredocs. Pinned in prepared commands.
    pub patch_config: ApplyPatchConfig,
    pub shell: Option<String>,
    pub default_login: bool,
    pub allow_shell_override: bool,
    pub allow_login_shell: bool,
    pub environment: EnvironmentVariables,
    pub pty_columns: u16,
    pub pty_rows: u16,
    /// A remote process lifetime. `None` matches Codex and allows the process to
    /// run until it exits, is terminated, or its execution host shuts down.
    pub process_timeout_ms: Option<u64>,
    pub max_output_bytes: usize,
    pub default_max_output_tokens: usize,
    pub max_output_tokens: usize,
    pub read_bytes: u64,
    /// Bounds one supervisor long-poll so callers can checkpoint frequently.
    pub max_poll_wait_ms: u64,
    pub max_empty_yield_time_ms: u64,
}

impl Default for UnifiedExecConfig {
    fn default() -> Self {
        Self {
            intercept_apply_patch: true,
            patch_config: ApplyPatchConfig::default(),
            shell: None,
            default_login: true,
            allow_shell_override: true,
            allow_login_shell: true,
            environment: EnvironmentVariables::default(),
            pty_columns: 80,
            pty_rows: 24,
            process_timeout_ms: None,
            max_output_bytes: DEFAULT_OUTPUT_MAX_BYTES,
            default_max_output_tokens: DEFAULT_MAX_OUTPUT_TOKENS,
            max_output_tokens: usize::MAX,
            read_bytes: 64 * 1024,
            max_poll_wait_ms: 1_000,
            max_empty_yield_time_ms: DEFAULT_MAX_EMPTY_YIELD_TIME_MS,
        }
    }
}

/// Caller-owned identities. Persist the prepared command before dispatch.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ExecCommandIds {
    pub session_id: i32,
    pub operation_id: OperationId,
    pub execution_id: ExecutionId,
    pub terminate_operation_id: OperationId,
}

/// Caller-owned identities for one stdin interaction.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct WriteStdinIds {
    pub write_id: WriteId,
    pub interrupt_operation_id: OperationId,
}

/// Session-scoped durable lookup state. The ID is an alias, not an OS PID.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ExecSession {
    pub schema_version: u32,
    pub session_id: i32,
    pub host_id: ExecutionHostId,
    pub handle: ExecutionHandle,
    pub cwd: ExecutionPath,
    pub tty: bool,
    pub cursor: u64,
    pub terminate_operation_id: OperationId,
}

impl ExecSession {
    #[must_use]
    pub fn is_running(&self) -> bool {
        !is_terminal(self.handle.state)
    }
}

/// Run-scoped durable state that must be committed before process start.
#[derive(Clone, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct PreparedExecCommand {
    #[serde(default)]
    patch_config: ApplyPatchConfig,
    host_id: ExecutionHostId,
    generation: SupervisorGenerationId,
    session_id: i32,
    request: StartExecutionRequest,
    terminate_operation_id: OperationId,
    yield_time_ms: u64,
    max_output_bytes: usize,
    max_output_tokens: Option<usize>,
    model_output_tokens: usize,
    history_output_tokens: usize,
    chunk_id: String,
    patch: Option<patch::Invocation>,
}

impl std::fmt::Debug for PreparedExecCommand {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("PreparedExecCommand")
            .field("host_id", &self.host_id)
            .field("generation", &self.generation)
            .field("session_id", &self.session_id)
            .field("execution_id", &self.request.execution_id)
            .field("tty", &matches!(self.request.stdin, StdinMode::Pty { .. }))
            .field("yield_time_ms", &self.yield_time_ms)
            .finish()
    }
}

impl PreparedExecCommand {
    #[must_use]
    pub fn host_id(&self) -> &ExecutionHostId {
        &self.host_id
    }

    #[must_use]
    pub fn generation(&self) -> &SupervisorGenerationId {
        &self.generation
    }

    #[must_use]
    pub const fn session_id(&self) -> i32 {
        self.session_id
    }

    #[must_use]
    pub fn request(&self) -> &StartExecutionRequest {
        &self.request
    }

    #[must_use]
    pub fn terminate_operation_id(&self) -> &OperationId {
        &self.terminate_operation_id
    }

    #[must_use]
    pub const fn effective_yield_time_ms(&self) -> u64 {
        self.yield_time_ms
    }
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
struct OutputCollection {
    started_at_ms: u64,
    deadline_at_ms: u64,
    finished_at_ms: Option<u64>,
    chunk_id: String,
    buffer: HeadTailBuffer,
    max_output_tokens: Option<usize>,
    model_output_tokens: usize,
    history_output_tokens: usize,
    failure: Option<String>,
    closed: bool,
    ready: bool,
}

impl OutputCollection {
    fn new(
        started_at_ms: u64,
        yield_time_ms: u64,
        max_output_bytes: usize,
        max_output_tokens: Option<usize>,
        model_output_tokens: usize,
        history_output_tokens: usize,
        chunk_id: String,
    ) -> Self {
        Self {
            started_at_ms,
            deadline_at_ms: started_at_ms.saturating_add(yield_time_ms),
            finished_at_ms: None,
            chunk_id,
            buffer: HeadTailBuffer::new(max_output_bytes),
            max_output_tokens,
            model_output_tokens,
            history_output_tokens,
            failure: None,
            closed: false,
            ready: false,
        }
    }

    fn finish(&mut self, at_ms: u64) {
        self.ready = true;
        self.finished_at_ms.get_or_insert(at_ms);
    }

    fn output(&self, session: &ExecSession) -> UnifiedExecOutput {
        let mut raw = self.buffer.text();
        if let Some(failure) = &self.failure {
            if !raw.is_empty() && !raw.ends_with('\n') {
                raw.push('\n');
            }
            raw.push_str(&format!("[Execution failed: {failure}]"));
        }
        let finished_at = self.finished_at_ms.unwrap_or_else(now_ms);
        let alive = session.is_running();
        UnifiedExecOutput {
            chunk_id: Some(self.chunk_id.clone()),
            wall_time_seconds: finished_at.saturating_sub(self.started_at_ms) as f64 / 1_000.0,
            exit_code: None,
            session_id: alive.then_some(session.session_id),
            original_token_count: Some(approximate_tokens(self.buffer.total_bytes())),
            output: raw,
            max_output_tokens: self.max_output_tokens,
            model_output_tokens: self.model_output_tokens,
            history_output_tokens: self.history_output_tokens,
            omitted_bytes: self.buffer.omitted_bytes(),
        }
    }
}

/// Run-scoped initial call state. Persist after start and every poll.
#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct RunningExecCommand {
    prepared: PreparedExecCommand,
    session: Option<ExecSession>,
    patch: Option<RunningPatch>,
    collection: OutputCollection,
    exit_code: Option<i32>,
}

impl RunningExecCommand {
    #[must_use]
    pub fn session(&self) -> Option<&ExecSession> {
        self.session.as_ref()
    }

    #[must_use]
    pub const fn ready(&self) -> bool {
        self.collection.ready
    }
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(tag = "type", rename_all = "snake_case", deny_unknown_fields)]
enum StdinAction {
    Poll,
    Write { request: WriteProcessInputRequest },
    Interrupt { request: SignalExecutionRequest },
}

/// Run-scoped state committed before stdin or interrupt dispatch.
#[derive(Clone, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct PreparedWriteStdin {
    host_id: ExecutionHostId,
    session: ExecSession,
    action: StdinAction,
    collection: OutputCollection,
}

impl std::fmt::Debug for PreparedWriteStdin {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let action = match &self.action {
            StdinAction::Poll => "poll",
            StdinAction::Write { .. } => "write",
            StdinAction::Interrupt { .. } => "interrupt",
        };
        formatter
            .debug_struct("PreparedWriteStdin")
            .field("host_id", &self.host_id)
            .field("session_id", &self.session.session_id)
            .field("action", &action)
            .field("yield_time_ms", &self.effective_yield_time_ms())
            .finish()
    }
}

impl PreparedWriteStdin {
    #[must_use]
    pub fn session(&self) -> &ExecSession {
        &self.session
    }

    #[must_use]
    pub fn write_request(&self) -> Option<&WriteProcessInputRequest> {
        match &self.action {
            StdinAction::Write { request } => Some(request),
            StdinAction::Poll | StdinAction::Interrupt { .. } => None,
        }
    }

    #[must_use]
    pub fn interrupt_request(&self) -> Option<&SignalExecutionRequest> {
        match &self.action {
            StdinAction::Interrupt { request } => Some(request),
            StdinAction::Poll | StdinAction::Write { .. } => None,
        }
    }

    #[must_use]
    pub const fn effective_yield_time_ms(&self) -> u64 {
        self.collection
            .deadline_at_ms
            .saturating_sub(self.collection.started_at_ms)
    }
}

/// Run-scoped stdin call state. Persist after interaction and every poll.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct RunningWriteStdin {
    prepared: PreparedWriteStdin,
    session: ExecSession,
    collection: OutputCollection,
    input_status: Option<ProcessInputStatus>,
    exit_code: Option<i32>,
}

impl RunningWriteStdin {
    #[must_use]
    pub fn session(&self) -> &ExecSession {
        &self.session
    }

    #[must_use]
    pub const fn input_status(&self) -> Option<ProcessInputStatus> {
        self.input_status
    }

    #[must_use]
    pub const fn ready(&self) -> bool {
        self.collection.ready
    }
}

/// Raw events allow a harness to checkpoint, stream, or archive every page.
pub struct UnifiedExecPoll {
    pub events: Vec<ProcessEvent>,
    pub ready: bool,
}

/// A completed model-facing call and the replacement session record.
#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct UnifiedExecCallResult {
    pub output: UnifiedExecOutput,
    /// Save this record when present; delete/tombstone the old record otherwise.
    pub session: Option<ExecSession>,
}

pub struct UnifiedExecTool<'a> {
    runtime: &'a dyn ExecutionRuntime,
    cwd: ExecutionPath,
    config: UnifiedExecConfig,
}

impl<'a> UnifiedExecTool<'a> {
    pub fn new(
        runtime: &'a dyn ExecutionRuntime,
        cwd: ExecutionPath,
        config: UnifiedExecConfig,
    ) -> ExecutionResult<Self> {
        validate_cwd(runtime.descriptor(), &cwd)?;
        config.environment.validate()?;
        if config.pty_columns == 0
            || config.pty_rows == 0
            || config.max_output_bytes == 0
            || config.default_max_output_tokens == 0
            || config.max_output_tokens == 0
            || config.default_max_output_tokens > config.max_output_tokens
            || config.read_bytes == 0
            || config.max_poll_wait_ms == 0
            || config.max_empty_yield_time_ms < MIN_EMPTY_YIELD_TIME_MS
            || config.process_timeout_ms == Some(0)
            || runtime.descriptor().limits.max_process_read_bytes == Some(0)
        {
            return Err(error(
                ExecutionErrorCode::InvalidRequest,
                "unified exec limits, PTY size, token budgets, and optional timeout must be positive and internally consistent",
            ));
        }
        Ok(Self {
            runtime,
            cwd,
            config,
        })
    }

    #[must_use]
    pub fn definitions(&self) -> Vec<ToolDefinition> {
        let mut schema = exec_command_input_schema();
        let properties = schema["properties"].as_object_mut().expect("object schema");
        if !self.config.allow_shell_override {
            properties.remove("shell");
        }
        if !self.config.allow_login_shell {
            properties.remove("login");
        } else if !self.config.default_login {
            properties.get_mut("login").expect("login schema")["description"] =
                json!("Whether to load shell login/profile configuration. Defaults to false.");
        }
        if self.runtime.descriptor().operating_system == OperatingSystem::Windows {
            properties.get_mut("yield_time_ms").expect("yield schema")["description"] = json!(
                "Wait before yielding output. Defaults to 10000 ms; effective range is 10000-30000 ms."
            );
        }
        vec![
            function_tool(EXEC_COMMAND_NAME, EXEC_COMMAND_DESCRIPTION, schema),
            write_stdin_definition(),
        ]
    }

    pub fn prepare_exec_command(
        &self,
        input: ExecCommandInput,
        ids: ExecCommandIds,
    ) -> ExecutionResult<PreparedExecCommand> {
        if ids.session_id <= 0 {
            return Err(error(
                ExecutionErrorCode::InvalidRequest,
                "session_id must be a positive integer",
            ));
        }
        if input.tty && !self.runtime.descriptor().features.pty {
            return Err(error(
                ExecutionErrorCode::Unsupported,
                "the selected execution host does not support PTYs",
            ));
        }
        if input.shell.is_some() && !self.config.allow_shell_override {
            return Err(error(
                ExecutionErrorCode::InvalidRequest,
                "shell overrides are disabled by the harness",
            ));
        }
        if input.login == Some(true) && !self.config.allow_login_shell {
            return Err(error(
                ExecutionErrorCode::InvalidRequest,
                "login shells are disabled by the harness",
            ));
        }
        let cwd = input
            .workdir
            .as_deref()
            .filter(|workdir| !workdir.is_empty())
            .map_or_else(
                || Ok(self.cwd.clone()),
                |workdir| resolve_path(self.runtime.descriptor(), &self.cwd, workdir),
            )?;
        let shell = input.shell.or_else(|| self.config.shell.clone());
        let login = input
            .login
            .unwrap_or(self.config.default_login && self.config.allow_login_shell);
        let patch = if self.config.intercept_apply_patch {
            patch::extract(&input.cmd)?
        } else {
            None
        }
        .map(|(body, workdir)| {
            let cwd = workdir.map_or_else(
                || Ok(cwd.clone()),
                |dir| resolve_path(self.runtime.descriptor(), &cwd, &dir),
            )?;
            Ok::<_, ExecutionError>(patch::Invocation { body, cwd })
        })
        .transpose()?;
        let request = StartExecutionRequest {
            expected_generation: Some(self.runtime.descriptor().supervisor_generation_id.clone()),
            output_drain_timeout_ms: Some(50),
            operation_id: ids.operation_id,
            execution_id: ids.execution_id,
            command: CommandSpec::ShellScript {
                command: input.cmd,
                shell,
                login,
            },
            cwd,
            environment: self.config.environment.clone(),
            stdin: if input.tty {
                StdinMode::Pty {
                    columns: self.config.pty_columns,
                    rows: self.config.pty_rows,
                }
            } else {
                StdinMode::Closed
            },
            timeout_ms: self.config.process_timeout_ms,
        };
        request.validate()?;
        let mut yield_time_ms = input
            .yield_time_ms
            .clamp(MIN_YIELD_TIME_MS, MAX_YIELD_TIME_MS);
        if self.runtime.descriptor().operating_system == OperatingSystem::Windows {
            yield_time_ms = yield_time_ms.max(DEFAULT_EXEC_YIELD_TIME_MS);
        }
        Ok(PreparedExecCommand {
            patch_config: self.config.patch_config.clone(),
            host_id: self.runtime.descriptor().host_id.clone(),
            generation: self.runtime.descriptor().supervisor_generation_id.clone(),
            session_id: ids.session_id,
            request,
            terminate_operation_id: ids.terminate_operation_id,
            yield_time_ms,
            max_output_bytes: self.config.max_output_bytes,
            max_output_tokens: input.max_output_tokens,
            model_output_tokens: self.resolve_max_output_tokens(input.max_output_tokens),
            history_output_tokens: self.config.max_output_tokens,
            chunk_id: generate_chunk_id(),
            patch,
        })
    }

    /// No automatic retry. Replay the same persisted value after uncertainty.
    pub async fn start_exec_command(
        &self,
        context: &OperationContext,
        prepared: &PreparedExecCommand,
    ) -> ExecutionResult<RunningExecCommand> {
        context.checkpoint()?;
        self.validate_prepared(prepared.host_id(), prepared.generation())?;
        prepared.request.validate()?;
        let started_at_ms = now_ms();
        if let Some(invocation) = &prepared.patch {
            let tool = ApplyPatchTool::new(
                self.runtime,
                invocation.cwd.clone(),
                prepared.patch_config.clone(),
            )?;
            let verified = tool
                .prepare(
                    context,
                    &invocation.body,
                    ApplyPatchState {
                        operation_id: prepared.request.operation_id.clone(),
                    },
                )
                .await?;
            return Ok(RunningExecCommand {
                prepared: prepared.clone(),
                session: None,
                patch: Some(tool.start(&verified)?),
                collection: OutputCollection::new(
                    started_at_ms,
                    prepared.yield_time_ms,
                    prepared.max_output_bytes,
                    prepared.max_output_tokens,
                    prepared.model_output_tokens,
                    prepared.history_output_tokens,
                    prepared.chunk_id.clone(),
                ),
                exit_code: None,
            });
        }
        let handle = self
            .runtime
            .processes()
            .start(context, prepared.request.clone())
            .await?;
        if handle.execution_id != prepared.request.execution_id
            || handle.supervisor_generation_id != prepared.generation
        {
            return Err(error(
                ExecutionErrorCode::Internal,
                "process start returned a different execution identity; outcome is uncertain",
            ));
        }
        let session = ExecSession {
            schema_version: SESSION_STATE_SCHEMA_VERSION,
            session_id: prepared.session_id,
            host_id: prepared.host_id.clone(),
            handle,
            cwd: prepared.request.cwd.clone(),
            tty: matches!(prepared.request.stdin, StdinMode::Pty { .. }),
            cursor: 0,
            terminate_operation_id: prepared.terminate_operation_id.clone(),
        };
        Ok(RunningExecCommand {
            prepared: prepared.clone(),
            session: Some(session),
            patch: None,
            collection: OutputCollection::new(
                started_at_ms,
                prepared.yield_time_ms,
                prepared.max_output_bytes,
                prepared.max_output_tokens,
                prepared.model_output_tokens,
                prepared.history_output_tokens,
                prepared.chunk_id.clone(),
            ),
            exit_code: None,
        })
    }

    pub async fn poll_exec_command(
        &self,
        context: &OperationContext,
        running: &mut RunningExecCommand,
    ) -> ExecutionResult<UnifiedExecPoll> {
        self.validate_prepared(running.prepared.host_id(), running.prepared.generation())?;
        if let Some(patch) = &mut running.patch {
            let invocation = running.prepared.patch.as_ref().expect("patch invocation");
            let tool = ApplyPatchTool::new(
                self.runtime,
                invocation.cwd.clone(),
                running.prepared.patch_config.clone(),
            )?;
            if let PatchProgress::Complete(output) = tool.step(context, patch).await? {
                if !running.collection.ready {
                    running.collection.buffer.push(output.to_text().as_bytes());
                    running.collection.finish(now_ms());
                }
            }
            return Ok(UnifiedExecPoll {
                events: vec![],
                ready: running.ready(),
            });
        }
        self.poll(
            context,
            running.session.as_mut().expect("process session"),
            &mut running.collection,
            &mut running.exit_code,
        )
        .await
    }

    pub fn finish_exec_command(
        &self,
        running: &RunningExecCommand,
    ) -> ExecutionResult<UnifiedExecCallResult> {
        if running.patch.is_some() {
            if !running.ready() {
                return Err(error(
                    ExecutionErrorCode::InvalidRequest,
                    "persist and poll the patch before finishing",
                ));
            }
            return Ok(UnifiedExecCallResult {
                output: UnifiedExecOutput {
                    chunk_id: None,
                    wall_time_seconds: 0.0,
                    exit_code: None,
                    session_id: None,
                    original_token_count: None,
                    output: running.collection.buffer.text(),
                    max_output_tokens: running.collection.max_output_tokens,
                    model_output_tokens: running.collection.model_output_tokens,
                    history_output_tokens: running.collection.history_output_tokens,
                    omitted_bytes: running.collection.buffer.omitted_bytes(),
                },
                session: None,
            });
        }
        self.finish(
            running.session.as_ref().expect("process session"),
            &running.collection,
            running.exit_code,
        )
    }

    /// Convenience for callers that do not need a checkpoint between pages.
    pub async fn wait_exec_command(
        &self,
        context: &OperationContext,
        running: &mut RunningExecCommand,
    ) -> ExecutionResult<UnifiedExecCallResult> {
        while !running.ready() {
            self.poll_exec_command(context, running).await?;
        }
        self.finish_exec_command(running)
    }

    pub fn prepare_write_stdin(
        &self,
        input: WriteStdinInput,
        session: ExecSession,
        ids: WriteStdinIds,
    ) -> ExecutionResult<PreparedWriteStdin> {
        self.validate_session(&session)?;
        if input.session_id != session.session_id {
            return Err(error(
                ExecutionErrorCode::InvalidRequest,
                "write_stdin session_id does not match the loaded session",
            ));
        }
        if !session.is_running() {
            return Err(error(
                ExecutionErrorCode::ExecutionNotFound,
                "the unified exec session has already completed",
            ));
        }
        let action = if input.chars.is_empty() {
            StdinAction::Poll
        } else if session.tty {
            let bytes = input.chars.into_bytes();
            let max = self
                .runtime
                .descriptor()
                .limits
                .max_process_input_bytes
                .unwrap_or(u64::MAX);
            if u64::try_from(bytes.len()).unwrap_or(u64::MAX) > max {
                return Err(error(
                    ExecutionErrorCode::ResourceExhausted,
                    "terminal input exceeds the execution host limit",
                ));
            }
            StdinAction::Write {
                request: WriteProcessInputRequest {
                    execution_id: session.handle.execution_id.clone(),
                    supervisor_generation_id: session.handle.supervisor_generation_id.clone(),
                    write_id: ids.write_id,
                    input: ProcessInput::Data {
                        data: BinaryData::new(bytes),
                    },
                },
            }
        } else if input.chars == "\u{3}" {
            if !self.runtime.descriptor().features.process_signals {
                return Err(error(
                    ExecutionErrorCode::Unsupported,
                    "the selected execution host does not support process signals",
                ));
            }
            StdinAction::Interrupt {
                request: SignalExecutionRequest {
                    operation_id: ids.interrupt_operation_id,
                    execution_id: session.handle.execution_id.clone(),
                    supervisor_generation_id: session.handle.supervisor_generation_id.clone(),
                    signal: ProcessSignal::Interrupt,
                },
            }
        } else {
            return Err(error(
                ExecutionErrorCode::StdinClosed,
                "stdin is closed for this session; rerun exec_command with tty=true to keep stdin open",
            ));
        };
        let empty = matches!(action, StdinAction::Poll);
        let yield_time_ms = if empty {
            input
                .yield_time_ms
                .clamp(MIN_EMPTY_YIELD_TIME_MS, self.config.max_empty_yield_time_ms)
        } else {
            input
                .yield_time_ms
                .clamp(MIN_YIELD_TIME_MS, MAX_YIELD_TIME_MS)
        };
        // Preparation stores a duration, never a live deadline.
        let started_at_ms = 0;
        Ok(PreparedWriteStdin {
            host_id: self.runtime.descriptor().host_id.clone(),
            session,
            action,
            collection: OutputCollection::new(
                started_at_ms,
                yield_time_ms,
                self.config.max_output_bytes,
                input.max_output_tokens,
                self.resolve_max_output_tokens(input.max_output_tokens),
                self.config.max_output_tokens,
                generate_chunk_id(),
            ),
        })
    }

    /// Applies exactly one persisted input action. Stable IDs make replay safe.
    pub async fn start_write_stdin(
        &self,
        context: &OperationContext,
        prepared: &PreparedWriteStdin,
    ) -> ExecutionResult<RunningWriteStdin> {
        context.checkpoint()?;
        self.validate_session(&prepared.session)?;
        let input_status = match &prepared.action {
            StdinAction::Poll => None,
            StdinAction::Write { request } => Some(
                self.runtime
                    .processes()
                    .write(context, request.clone())
                    .await?
                    .status,
            ),
            StdinAction::Interrupt { request } => {
                if let Err(failure) = self
                    .runtime
                    .processes()
                    .signal(context, request.clone())
                    .await
                    && failure.code != ExecutionErrorCode::ExecutionNotFound
                {
                    return Err(failure);
                }
                None
            }
        };
        if matches!(prepared.action, StdinAction::Write { .. })
            && matches!(
                input_status,
                Some(ProcessInputStatus::Accepted | ProcessInputStatus::AlreadyAccepted)
            )
        {
            tokio::time::sleep(std::time::Duration::from_millis(100)).await;
            context.checkpoint()?;
        }
        let mut collection = prepared.collection.clone();
        collection.started_at_ms = now_ms();
        collection.deadline_at_ms = collection
            .started_at_ms
            .saturating_add(prepared.effective_yield_time_ms());
        Ok(RunningWriteStdin {
            prepared: prepared.clone(),
            session: prepared.session.clone(),
            collection,
            input_status,
            exit_code: None,
        })
    }

    pub async fn poll_write_stdin(
        &self,
        context: &OperationContext,
        running: &mut RunningWriteStdin,
    ) -> ExecutionResult<UnifiedExecPoll> {
        self.validate_session(&running.session)?;
        self.poll(
            context,
            &mut running.session,
            &mut running.collection,
            &mut running.exit_code,
        )
        .await
    }

    pub fn finish_write_stdin(
        &self,
        running: &RunningWriteStdin,
    ) -> ExecutionResult<UnifiedExecCallResult> {
        if running.input_status == Some(ProcessInputStatus::StdinClosed)
            && running.session.is_running()
        {
            return Err(error(
                ExecutionErrorCode::StdinClosed,
                "stdin closed before the terminal input could be accepted",
            ));
        }
        self.finish(&running.session, &running.collection, running.exit_code)
    }

    pub async fn wait_write_stdin(
        &self,
        context: &OperationContext,
        running: &mut RunningWriteStdin,
    ) -> ExecutionResult<UnifiedExecCallResult> {
        while !running.ready() {
            self.poll_write_stdin(context, running).await?;
        }
        self.finish_write_stdin(running)
    }

    /// Explicit cleanup using the session's stable termination identity.
    pub async fn terminate_session(
        &self,
        context: &OperationContext,
        session: &ExecSession,
    ) -> ExecutionResult<TerminateExecutionResult> {
        self.validate_session(session)?;
        self.runtime
            .processes()
            .terminate(
                context,
                TerminateExecutionRequest {
                    operation_id: session.terminate_operation_id.clone(),
                    execution_id: session.handle.execution_id.clone(),
                    supervisor_generation_id: session.handle.supervisor_generation_id.clone(),
                },
            )
            .await
    }

    async fn poll(
        &self,
        context: &OperationContext,
        session: &mut ExecSession,
        collection: &mut OutputCollection,
        exit_code: &mut Option<i32>,
    ) -> ExecutionResult<UnifiedExecPoll> {
        if collection.ready {
            return Ok(UnifiedExecPoll {
                events: Vec::new(),
                ready: true,
            });
        }
        context.checkpoint()?;
        let now = now_ms();
        let remaining = collection.deadline_at_ms.saturating_sub(now);
        let wait_ms = if remaining == 0 && !is_terminal(session.handle.state) {
            None
        } else {
            Some(if is_terminal(session.handle.state) {
                50
            } else {
                remaining.min(self.config.max_poll_wait_ms).max(1)
            })
        };
        let reply = self
            .runtime
            .processes()
            .read(
                context,
                ReadExecutionRequest {
                    execution_id: session.handle.execution_id.clone(),
                    supervisor_generation_id: session.handle.supervisor_generation_id.clone(),
                    after_sequence: session.cursor,
                    max_bytes: self.config.read_bytes.min(
                        self.runtime
                            .descriptor()
                            .limits
                            .max_process_read_bytes
                            .unwrap_or(u64::MAX),
                    ),
                    wait_ms,
                },
            )
            .await?;

        let mut cursor = session.cursor;
        for event in &reply.events {
            if event.sequence <= cursor {
                return Err(error(
                    ExecutionErrorCode::Internal,
                    "process events did not advance in sequence",
                ));
            }
            cursor = event.sequence;
        }
        if reply.next_sequence != cursor {
            return Err(error(
                ExecutionErrorCode::Internal,
                "process cursor does not match returned events",
            ));
        }

        for event in &reply.events {
            match &event.event {
                ProcessEventKind::Output { data, .. } => collection.buffer.push(data.as_slice()),
                ProcessEventKind::Failed { message } => collection.failure = Some(message.clone()),
                ProcessEventKind::Closed => collection.closed = true,
                ProcessEventKind::Started | ProcessEventKind::Exited { .. } => {}
            }
        }
        session.cursor = cursor;
        session.handle.state = reply.state;
        *exit_code = reply.exit_code;

        let now = now_ms();
        if (is_terminal(reply.state) && collection.closed)
            || (!is_terminal(reply.state) && now >= collection.deadline_at_ms)
        {
            collection.finish(now);
        }
        Ok(UnifiedExecPoll {
            events: reply.events,
            ready: collection.ready,
        })
    }

    fn finish(
        &self,
        session: &ExecSession,
        collection: &OutputCollection,
        exit_code: Option<i32>,
    ) -> ExecutionResult<UnifiedExecCallResult> {
        if !collection.ready {
            return Err(error(
                ExecutionErrorCode::InvalidRequest,
                "the unified exec call is not ready; poll and persist it before finishing",
            ));
        }
        let mut output = collection.output(session);
        if !session.is_running() {
            output.exit_code = exit_code;
        }
        Ok(UnifiedExecCallResult {
            output,
            session: session.is_running().then(|| session.clone()),
        })
    }

    fn validate_prepared(
        &self,
        host_id: &ExecutionHostId,
        generation: &SupervisorGenerationId,
    ) -> ExecutionResult<()> {
        if host_id != &self.runtime.descriptor().host_id {
            return Err(error(
                ExecutionErrorCode::InvalidRequest,
                "unified exec state belongs to another execution host",
            ));
        }
        if generation != &self.runtime.descriptor().supervisor_generation_id {
            return Err(error(
                ExecutionErrorCode::ExecutionLost,
                "the execution supervisor restarted and the unified exec session was lost",
            ));
        }
        Ok(())
    }

    fn validate_session(&self, session: &ExecSession) -> ExecutionResult<()> {
        if session.schema_version != SESSION_STATE_SCHEMA_VERSION || session.session_id <= 0 {
            return Err(error(
                ExecutionErrorCode::InvalidRequest,
                "invalid or unsupported unified exec session state",
            ));
        }
        self.validate_prepared(&session.host_id, &session.handle.supervisor_generation_id)
    }

    fn resolve_max_output_tokens(&self, requested: Option<usize>) -> usize {
        requested
            .unwrap_or(self.config.default_max_output_tokens)
            .min(self.config.max_output_tokens)
    }
}

#[must_use]
pub fn definitions() -> Vec<ToolDefinition> {
    vec![exec_command_definition(), write_stdin_definition()]
}

#[must_use]
pub fn exec_command_definition() -> ToolDefinition {
    function_tool(
        EXEC_COMMAND_NAME,
        EXEC_COMMAND_DESCRIPTION,
        exec_command_input_schema(),
    )
}

#[must_use]
pub fn write_stdin_definition() -> ToolDefinition {
    function_tool(
        WRITE_STDIN_NAME,
        WRITE_STDIN_DESCRIPTION,
        write_stdin_input_schema(),
    )
}

#[must_use]
pub fn exec_command_input_schema() -> Value {
    json!({
        "type": "object",
        "properties": {
            "cmd": {"type":"string", "description":"Shell command to execute."},
            "workdir": {"type":"string", "description":"Working directory for the command. Defaults to the turn cwd."},
            "tty": {"type":"boolean", "description":"True allocates a PTY for the command; false or omitted uses plain pipes."},
            "yield_time_ms": {"type":"number", "description":"Wait before yielding output. Defaults to 10000 ms; effective range is 250-30000 ms."},
            "max_output_tokens": {"type":"number", "description":"Output token budget. Defaults to 10000 tokens; larger requests may be capped by policy."},
            "shell": {"type":"string", "description":"Shell binary to launch. Defaults to the user's default shell."},
            "login": {"type":"boolean", "description":"True runs the shell with -l/-i semantics; false disables them. Defaults to true."}
        },
        "required": ["cmd"],
        "additionalProperties": false
    })
}

#[must_use]
pub fn write_stdin_input_schema() -> Value {
    json!({
        "type": "object",
        "properties": {
            "session_id": {"type":"number", "description":"Identifier of the running unified exec session."},
            "chars": {"type":"string", "description":"Bytes to write to stdin. Defaults to empty, which polls without writing."},
            "yield_time_ms": {"type":"number", "description":"Wait before yielding output. Non-empty writes default to 250 ms and cap at 30000 ms; empty polls wait 5000-300000 ms by default."},
            "max_output_tokens": {"type":"number", "description":"Output token budget. Defaults to 10000 tokens; larger requests may be capped by policy."}
        },
        "required": ["session_id"],
        "additionalProperties": false
    })
}

#[must_use]
pub fn output_schema() -> Value {
    json!({
        "type": "object",
        "properties": {
            "chunk_id": {"type":"string", "description":"Chunk identifier included when the response reports one."},
            "wall_time_seconds": {"type":"number", "description":"Elapsed wall time spent waiting for output in seconds."},
            "exit_code": {"type":"number", "description":"Process exit code when the command finished during this call."},
            "session_id": {"type":"number", "description":"Session identifier to pass to write_stdin when the process is still running."},
            "original_token_count": {"type":"number", "description":"Approximate token count before output truncation."},
            "output": {"type":"string", "description":"Command output text, possibly truncated."}
        },
        "required": ["wall_time_seconds", "output"],
        "additionalProperties": false
    })
}

pub fn parse_exec_command_arguments(
    arguments: &ToolArguments,
) -> Result<ExecCommandInput, serde_json::Error> {
    parse_arguments(arguments)
}

pub fn parse_write_stdin_arguments(
    arguments: &ToolArguments,
) -> Result<WriteStdinInput, serde_json::Error> {
    parse_arguments(arguments)
}

fn parse_arguments<T: DeserializeOwned>(arguments: &ToolArguments) -> Result<T, serde_json::Error> {
    match arguments {
        ToolArguments::Object(arguments) => {
            serde_json::from_value(Value::Object(arguments.clone()))
        }
        ToolArguments::String(arguments) => serde_json::from_str(arguments),
    }
}

fn function_tool(name: &str, description: &str, parameters: Value) -> ToolDefinition {
    let Value::Object(parameters) = parameters else {
        unreachable!("tool parameters are declared as an object")
    };
    let Value::Object(output_schema) = output_schema() else {
        unreachable!("tool output is declared as an object")
    };
    ToolDefinition::Function(FunctionTool {
        name: name.to_string(),
        description: description.to_string(),
        parameters,
        output_schema: Some(output_schema),
        strict: Some(false),
    })
}

fn now_ms() -> u64 {
    TimestampMs::now().0
}

fn generate_chunk_id() -> String {
    let mut random = rand::thread_rng();
    (0..6)
        .map(|_| format!("{:x}", random.gen_range(0..16)))
        .collect()
}

fn is_terminal(state: ExecutionState) -> bool {
    !matches!(state, ExecutionState::Starting | ExecutionState::Running)
}

fn error(code: ExecutionErrorCode, message: impl Into<String>) -> ExecutionError {
    ExecutionError::new(code, message).with_detail("source", "tool-unified-exec")
}
