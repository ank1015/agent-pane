#![doc = include_str!("../README.md")]

mod output;
mod shell;

use std::time::Duration;

use execution_core::*;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

pub use output::BashOutput;
use output::Tail;

pub const NAME: &str = "bash-minimal";
pub const DEFAULT_TIMEOUT_MS: u64 = 120_000;
pub const MAX_TIMEOUT_MS: u64 = 1_800_000;
pub const DESCRIPTION: &str = "Execute a shell command on the execution host and wait for completion. Unless the harness specifies an override, Windows uses Windows PowerShell (powershell.exe, no profile, noninteractive); Unix uses the supervisor's default shell. Use PowerShell syntax on Windows, not cmd.exe or Bash syntax; do not assume PowerShell 7 features such as &&. Each call uses a fresh shell with closed stdin and no PTY. Shell variables, functions, and directory changes do not persist between calls; filesystem changes do. Use workdir to select a directory; it defaults to the environment directory. Relative workdir paths resolve against that directory; absolute paths must be within a registered root. Parent (..) segments are unsupported; use an absolute path instead. Timeout is in milliseconds (default 120000, maximum 1800000) and terminates the remote command. Returns combined stdout/stderr and exit status. Output is bounded; truncated output is explicitly marked. No background or interactive-input mode is provided. Use read, write, and edit for file operations.";

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct BashInput {
    pub command: String,
    pub timeout: Option<u64>,
    pub workdir: Option<String>,
}

pub fn input_schema() -> Value {
    json!({"type":"object","properties":{
        "command":{"type":"string","minLength":1,"description":"Shell command to execute."},
        "timeout":{"type":"integer","minimum":1,"maximum":MAX_TIMEOUT_MS,"description":"Timeout in milliseconds. Defaults to 120000; maximum 1800000."},
        "workdir":{"type":"string","minLength":1,"description":"Working directory on the execution host. Defaults to the environment directory; relative paths resolve against it. Use this instead of cd."}
    },"required":["command"],"additionalProperties":false})
}

/// Host-side shell configuration and output limits, never model arguments.
#[derive(Clone, Debug)]
pub struct BashConfig {
    pub shell: Option<String>,
    pub login: bool,
    pub environment: EnvironmentVariables,
    pub max_output_bytes: usize,
    pub max_output_lines: usize,
    pub read_bytes: u64,
}

impl Default for BashConfig {
    fn default() -> Self {
        Self {
            shell: None,
            login: false,
            environment: EnvironmentVariables::default(),
            max_output_bytes: 50 * 1024,
            max_output_lines: 2000,
            read_bytes: 64 * 1024,
        }
    }
}

/// Caller-owned identities. Persist the prepared request before starting it.
#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct BashIds {
    pub operation_id: OperationId,
    pub execution_id: ExecutionId,
    pub terminate_operation_id: OperationId,
}

/// Trusted durable state, not model input. Debug deliberately omits command/env.
#[derive(Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct PreparedBash {
    host_id: ExecutionHostId,
    generation: SupervisorGenerationId,
    request: StartExecutionRequest,
    terminate_operation_id: OperationId,
    max_output_bytes: usize,
    max_output_lines: usize,
}

impl PreparedBash {
    /// Generation captured before dispatch, for reconciling an uncertain start.
    pub fn generation(&self) -> &SupervisorGenerationId {
        &self.generation
    }
    pub fn terminate_operation_id(&self) -> &OperationId {
        &self.terminate_operation_id
    }
    pub fn request(&self) -> &StartExecutionRequest {
        &self.request
    }
}

/// Persist after start and after each poll, atomically with consumed events.
#[derive(Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct RunningBash {
    prepared: PreparedBash,
    handle: ExecutionHandle,
    cursor: u64,
    tail: Tail,
    exit_code: Option<i32>,
    failure: Option<String>,
    complete: bool,
}

impl RunningBash {
    pub fn handle(&self) -> &ExecutionHandle {
        &self.handle
    }
    pub fn cursor(&self) -> u64 {
        self.cursor
    }
    /// A bounded snapshot, including partial output when polling fails.
    pub fn output(&self) -> BashOutput {
        BashOutput {
            host_id: self.prepared.host_id.clone(),
            handle: self.handle.clone(),
            cwd: self.prepared.request.cwd.clone(),
            output: self.tail.text(),
            truncated: self.tail.truncated,
            output_bytes: self.tail.total_bytes,
            exit_code: self.exit_code,
            failure: self.failure.clone(),
            complete: self.complete,
        }
    }
}

/// Raw events allow a harness to stream UI updates or archive full output.
pub struct BashPoll {
    pub events: Vec<ProcessEvent>,
    pub complete: bool,
}

pub struct BashTool<'a> {
    runtime: &'a dyn ExecutionRuntime,
    cwd: ExecutionPath,
    config: BashConfig,
}

impl<'a> BashTool<'a> {
    pub fn new(
        runtime: &'a dyn ExecutionRuntime,
        cwd: ExecutionPath,
        config: BashConfig,
    ) -> ExecutionResult<Self> {
        tool_filesystem::validate_cwd(runtime.descriptor(), &cwd)?;
        config.environment.validate()?;
        if config.max_output_bytes == 0
            || config.max_output_lines == 0
            || config.read_bytes == 0
            || runtime.descriptor().limits.max_process_read_bytes == Some(0)
        {
            return Err(error(
                ExecutionErrorCode::InvalidRequest,
                "output and read limits must be positive",
            ));
        }
        Ok(Self {
            runtime,
            cwd,
            config,
        })
    }

    pub fn prepare(&self, input: BashInput, ids: BashIds) -> ExecutionResult<PreparedBash> {
        if input.command.trim().is_empty() {
            return Err(error(
                ExecutionErrorCode::InvalidRequest,
                "command must be nonempty",
            ));
        }
        let timeout = input.timeout.unwrap_or(DEFAULT_TIMEOUT_MS);
        if timeout == 0 || timeout > MAX_TIMEOUT_MS {
            return Err(error(
                ExecutionErrorCode::InvalidRequest,
                "timeout must be between 1 and 1800000 milliseconds",
            ));
        }
        if ids.operation_id == ids.terminate_operation_id {
            return Err(error(
                ExecutionErrorCode::InvalidRequest,
                "start and terminate require different operation IDs",
            ));
        }
        let cwd = match input.workdir {
            Some(path) => {
                tool_filesystem::resolve_path(self.runtime.descriptor(), &self.cwd, &path)?
            }
            None => self.cwd.clone(),
        };
        let request = StartExecutionRequest {
            expected_generation: None,
            output_drain_timeout_ms: None,
            operation_id: ids.operation_id,
            execution_id: ids.execution_id,
            command: shell::command(
                self.runtime.descriptor().operating_system.clone(),
                &self.config,
                input.command,
            ),
            cwd,
            environment: self.config.environment.clone(),
            stdin: StdinMode::Closed,
            timeout_ms: Some(timeout),
        };
        request.validate()?;
        Ok(PreparedBash {
            host_id: self.runtime.descriptor().host_id.clone(),
            generation: self.runtime.descriptor().supervisor_generation_id.clone(),
            request,
            terminate_operation_id: ids.terminate_operation_id,
            max_output_bytes: self.config.max_output_bytes,
            max_output_lines: self.config.max_output_lines,
        })
    }

    /// No automatic retries. On ambiguous failure retain/reconcile this request.
    pub async fn start(
        &self,
        context: &OperationContext,
        prepared: &PreparedBash,
    ) -> ExecutionResult<RunningBash> {
        context.checkpoint()?;
        self.validate_host(prepared)?;
        if prepared.generation != self.runtime.descriptor().supervisor_generation_id {
            return Err(error(
                ExecutionErrorCode::ExecutionLost,
                "supervisor generation changed; reconcile the old execution before starting again",
            ));
        }
        prepared.request.validate()?;
        let handle = self
            .runtime
            .processes()
            .start(context, prepared.request.clone())
            .await?;
        if handle.execution_id != prepared.request.execution_id {
            return Err(error(
                ExecutionErrorCode::Internal,
                "start returned another execution ID; command outcome is uncertain",
            ));
        }
        // Use the returned generation, never replace it from a refreshed descriptor.
        Ok(RunningBash {
            prepared: prepared.clone(),
            handle,
            cursor: 0,
            tail: Tail::new(prepared.max_output_bytes, prepared.max_output_lines),
            exit_code: None,
            failure: None,
            complete: false,
        })
    }

    /// One bounded long-poll. Never starts/restarts an execution. Transport errors
    /// leave the saved cursor and tail unchanged, allowing deliberate reconnection.
    pub async fn poll(
        &self,
        context: &OperationContext,
        running: &mut RunningBash,
    ) -> ExecutionResult<BashPoll> {
        self.validate_host(&running.prepared)?;
        if running.complete {
            return Ok(BashPoll {
                events: Vec::new(),
                complete: true,
            });
        }
        context.checkpoint()?;
        let reply = self
            .runtime
            .processes()
            .read(
                context,
                ReadExecutionRequest {
                    execution_id: running.handle.execution_id.clone(),
                    supervisor_generation_id: running.handle.supervisor_generation_id.clone(),
                    after_sequence: running.cursor,
                    max_bytes: self.config.read_bytes.min(
                        self.runtime
                            .descriptor()
                            .limits
                            .max_process_read_bytes
                            .unwrap_or(u64::MAX),
                    ),
                    wait_ms: Some(1000),
                },
            )
            .await?;
        // Validate before mutating any durable state. Sequences can span bytes,
        // so they must increase but need not increase by exactly one.
        let mut cursor = running.cursor;
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
                ProcessEventKind::Output { data, .. } => running.tail.push(data.as_slice()),
                ProcessEventKind::Failed { message } => running.failure = Some(message.clone()),
                _ => {}
            }
        }
        running.cursor = cursor;
        running.handle.state = reply.state;
        running.exit_code = reply.exit_code;
        // State is the journal's current state, not the state at this page's
        // cursor. Drain all pages, including output produced before an exit.
        running.complete = is_terminal(reply.state)
            && (reply.events.is_empty()
                || reply
                    .events
                    .iter()
                    .any(|event| matches!(event.event, ProcessEventKind::Closed)));
        Ok(BashPoll {
            events: reply.events,
            complete: running.complete,
        })
    }

    /// Foreground convenience loop. For checkpointing/streaming, call poll and
    /// persist RunningBash after each page instead. Errors preserve partial state.
    pub async fn wait(
        &self,
        context: &OperationContext,
        running: &mut RunningBash,
    ) -> ExecutionResult<BashOutput> {
        loop {
            match self.poll(context, running).await {
                Ok(page) if page.complete => return Ok(running.output()),
                Ok(_) => {}
                Err(mut failure) => {
                    if context.checkpoint().is_err() {
                        // The cancelled/expired request context cannot send cleanup.
                        let cleanup = OperationContext::with_timeout(Duration::from_secs(5));
                        if let Err(stop_error) = self.terminate(&cleanup, running).await {
                            failure = failure.with_detail(
                                "termination_error",
                                serde_json::to_value(stop_error).unwrap_or(Value::Null),
                            );
                        }
                    }
                    return Err(failure);
                }
            }
        }
    }

    /// Explicit harness abort. Dropping a future or crashing a worker does not
    /// terminate the remote process. Its remote timeout remains in force.
    pub async fn terminate(
        &self,
        context: &OperationContext,
        running: &RunningBash,
    ) -> ExecutionResult<TerminateExecutionResult> {
        self.validate_host(&running.prepared)?;
        self.runtime
            .processes()
            .terminate(
                context,
                TerminateExecutionRequest {
                    operation_id: running.prepared.terminate_operation_id.clone(),
                    execution_id: running.handle.execution_id.clone(),
                    supervisor_generation_id: running.handle.supervisor_generation_id.clone(),
                },
            )
            .await
    }

    fn validate_host(&self, prepared: &PreparedBash) -> ExecutionResult<()> {
        if prepared.host_id != self.runtime.descriptor().host_id {
            return Err(error(
                ExecutionErrorCode::InvalidRequest,
                "execution belongs to another host",
            ));
        }
        Ok(())
    }
}

fn is_terminal(state: ExecutionState) -> bool {
    !matches!(state, ExecutionState::Starting | ExecutionState::Running)
}

fn error(code: ExecutionErrorCode, message: impl Into<String>) -> ExecutionError {
    ExecutionError::new(code, message).with_detail("source", "tool-bash-minimal")
}
