use std::{sync::Arc, time::Duration};

use base64::{Engine as _, engine::general_purpose::STANDARD};
use execution_contracts::{
    AttachExecutionRequest, Base64Data, CommandSpec, EnvironmentInheritance, EnvironmentVariables,
    ExecutionErrorCode, ExecutionId, ExecutionPersistence, ExecutionPolicy, ExecutionState,
    InspectExecutionRequest, NetworkMode, OperationId, ProcessEventKind, ProcessInputStatus,
    ProcessOutputPolicy, ProcessSignal, SandboxMode, SignalExecutionRequest, StartExecutionRequest,
    StdinMode, WriteProcessInputRequest,
};
use execution_runtime::ProcessRuntime;
use futures_util::StreamExt as _;
use llm_contracts::ToolArguments;
use serde::{Serialize, de::DeserializeOwned};
use serde_json::Value;
use uuid::Uuid;

use crate::{
    CodexExecSession, CodexExecToolContext, CodexExecToolError, CodexExecToolOutput,
    DEFAULT_MAX_OUTPUT_TOKENS, ExecCommandArguments, MAX_YIELD_TIME_MS,
    MIN_EMPTY_POLL_YIELD_TIME_MS, MIN_YIELD_TIME_MS, WriteStdinArguments,
    output::{HeadTailBuffer, approx_token_count_from_bytes, model_output},
};

const OUTPUT_CHUNK_BYTES: u32 = 16 * 1024;
const INTERRUPT: &str = "\u{3}";

/// Parses provider-neutral arguments for `exec_command`.
pub fn parse_exec_command_arguments(
    arguments: &ToolArguments,
) -> Result<ExecCommandArguments, CodexExecToolError> {
    parse(arguments).map_err(|error| {
        CodexExecToolError::invalid_arguments(format!(
            "Invalid arguments for exec_command: {error}"
        ))
    })
}

/// Parses provider-neutral arguments for `write_stdin`.
pub fn parse_write_stdin_arguments(
    arguments: &ToolArguments,
) -> Result<WriteStdinArguments, CodexExecToolError> {
    parse(arguments).map_err(|error| {
        CodexExecToolError::invalid_arguments(format!("Invalid arguments for write_stdin: {error}"))
    })
}

/// Parses and executes one `exec_command` call.
pub async fn execute_exec_command_tool(
    arguments: &ToolArguments,
    context: &CodexExecToolContext<'_>,
) -> Result<CodexExecToolOutput, CodexExecToolError> {
    execute_exec_command(parse_exec_command_arguments(arguments)?, context).await
}

/// Parses and executes one `write_stdin` call.
pub async fn execute_write_stdin_tool(
    arguments: &ToolArguments,
    context: &CodexExecToolContext<'_>,
) -> Result<CodexExecToolOutput, CodexExecToolError> {
    execute_write_stdin(parse_write_stdin_arguments(arguments)?, context).await
}

/// Starts a shell command, waits for its initial yield, and either reports
/// completion or leaves a harness-owned session mapping for `write_stdin`.
pub async fn execute_exec_command(
    arguments: ExecCommandArguments,
    context: &CodexExecToolContext<'_>,
) -> Result<CodexExecToolOutput, CodexExecToolError> {
    if arguments.cmd.is_empty() {
        return Err(CodexExecToolError::invalid_arguments(
            "Invalid arguments for exec_command: cmd must not be empty",
        ));
    }
    if arguments.shell.as_ref().is_some_and(String::is_empty) {
        return Err(CodexExecToolError::invalid_arguments(
            "Invalid arguments for exec_command: shell must not be empty",
        ));
    }
    let options = context.options();
    let login = match arguments.login {
        Some(true) if !options.allow_login_shell => {
            return Err(CodexExecToolError::invalid_arguments(
                "login shell is disabled by config; omit `login` or set it to false.",
            ));
        }
        Some(login) => login,
        None => options.allow_login_shell,
    };
    let runtime = process_runtime(context)?;
    let cwd = context.resolve_workdir(arguments.workdir.as_deref())?;
    let execution_id = ExecutionId::new(format!("codex-exec-{}", Uuid::now_v7()))
        .expect("UUID execution identifier is valid");
    let session = context
        .sessions()
        .insert(execution_id.clone(), arguments.tty)
        .await?;
    if session.execution_id() != &execution_id || session.tty() != arguments.tty {
        let _ = remove_session(context, &session).await;
        return Err(CodexExecToolError::process(
            "session store returned a handle that does not match the inserted execution",
        ));
    }
    let mut interaction = session.lock_interaction().await;

    let stdin = if arguments.tty {
        StdinMode::Pty {
            columns: options.pty_columns,
            rows: options.pty_rows,
        }
    } else {
        StdinMode::Closed
    };
    let start_result = runtime
        .start(
            context.operation(),
            StartExecutionRequest {
                operation_id: operation_id("codex-exec-start", context.tool_call_id()),
                execution_id: execution_id.clone(),
                command: CommandSpec::Shell {
                    command: arguments.cmd,
                    shell: arguments.shell,
                    login,
                },
                cwd,
                environment: EnvironmentVariables {
                    inherit: EnvironmentInheritance::All,
                    allow: Vec::new(),
                    remove: Vec::new(),
                    set: Default::default(),
                },
                stdin,
                timeout_ms: None,
                persistence: ExecutionPersistence::KeepUntilExit,
                policy: ExecutionPolicy {
                    sandbox: SandboxMode::Disabled,
                    network: NetworkMode::Inherit,
                    profile: None,
                    resource_limits: None,
                },
                output: ProcessOutputPolicy {
                    persist_full_output: true,
                    max_inline_bytes: crate::OUTPUT_COLLECTION_MAX_BYTES as u64,
                    max_chunk_bytes: OUTPUT_CHUNK_BYTES,
                },
            },
        )
        .await;
    if let Err(error) = start_result {
        drop(interaction);
        remove_session(context, &session).await?;
        return Err(error.into());
    }

    let collected = collect(
        runtime,
        context,
        &session,
        &mut interaction,
        clamp_initial_yield(arguments.yield_time_ms),
    )
    .await;
    let cursor_result = context
        .sessions()
        .update_last_sequence(
            session.session_id(),
            session.execution_id(),
            interaction.last_sequence,
        )
        .await;
    drop(interaction);
    cursor_result?;
    let collected = match collected {
        Ok(collected) => collected,
        Err(error) => {
            // Codex publishes the process into its session store before the
            // initial yield and intentionally leaves it running when the outer
            // turn is cancelled. Preserve both the process and its mapping;
            // process-lifetime cleanup remains the session owner's responsibility.
            if error.name() != "cancelled" {
                cleanup_terminal_session(runtime, context, &session).await;
            }
            return Err(error);
        }
    };
    finish_output(context, session, collected, arguments.max_output_tokens).await
}

/// Writes to or polls one harness-owned process session.
pub async fn execute_write_stdin(
    arguments: WriteStdinArguments,
    context: &CodexExecToolContext<'_>,
) -> Result<CodexExecToolOutput, CodexExecToolError> {
    let Some(session) = context.sessions().get(arguments.session_id).await? else {
        return Err(CodexExecToolError::process(format!(
            "write_stdin failed: unknown session ID {}",
            arguments.session_id
        )));
    };
    let runtime = process_runtime(context)?;
    let mut interaction = session.lock_interaction().await;

    if !arguments.chars.is_empty() {
        if !session.tty() && arguments.chars == INTERRUPT {
            runtime
                .signal(
                    context.operation(),
                    SignalExecutionRequest {
                        execution_id: session.execution_id().clone(),
                        signal: ProcessSignal::Interrupt,
                    },
                )
                .await?;
        } else {
            let write = runtime
                .write_input(
                    context.operation(),
                    WriteProcessInputRequest {
                        execution_id: session.execution_id().clone(),
                        write_id: operation_id("codex-write-stdin", context.tool_call_id()),
                        data: Base64Data(STANDARD.encode(arguments.chars.as_bytes())),
                    },
                )
                .await;
            let status = match write {
                Ok(write) => write.status,
                Err(error)
                    if matches!(
                        error.code,
                        ExecutionErrorCode::StdinClosed | ExecutionErrorCode::UnknownExecution
                    ) =>
                {
                    let terminal = inspect_is_terminal(runtime, context, &session)
                        .await
                        .unwrap_or(false);
                    if terminal {
                        ProcessInputStatus::StdinClosed
                    } else {
                        return Err(error.into());
                    }
                }
                Err(error) => return Err(error.into()),
            };
            match status {
                ProcessInputStatus::Accepted => {}
                ProcessInputStatus::UnknownExecution => {
                    drop(interaction);
                    remove_session(context, &session).await?;
                    return Err(CodexExecToolError::process(format!(
                        "write_stdin failed: unknown session ID {}",
                        arguments.session_id
                    )));
                }
                ProcessInputStatus::StdinClosed => {
                    if !inspect_is_terminal(runtime, context, &session).await? {
                        return Err(CodexExecToolError::process(
                            "write_stdin failed: stdin is closed for this session",
                        ));
                    }
                }
                ProcessInputStatus::Starting => {
                    return Err(CodexExecToolError::process(
                        "write_stdin failed: process is still starting",
                    ));
                }
            }
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }

    let yield_time_ms = clamp_write_yield(
        arguments.yield_time_ms,
        arguments.chars.is_empty(),
        context.options().max_empty_poll_yield_time_ms,
    );
    let collected = collect(runtime, context, &session, &mut interaction, yield_time_ms).await;
    let cursor_result = context
        .sessions()
        .update_last_sequence(
            session.session_id(),
            session.execution_id(),
            interaction.last_sequence,
        )
        .await;
    drop(interaction);
    cursor_result?;
    let collected = match collected {
        Ok(collected) => collected,
        Err(error) => {
            cleanup_terminal_session(runtime, context, &session).await;
            return Err(error);
        }
    };
    finish_output(context, session, collected, arguments.max_output_tokens).await
}

fn process_runtime<'a>(
    context: &'a CodexExecToolContext<'_>,
) -> Result<&'a dyn ProcessRuntime, CodexExecToolError> {
    context
        .runtime()
        .process_runtime()
        .ok_or_else(|| CodexExecToolError::missing_capability("process sessions"))
}

struct CollectedOutput {
    buffer: HeadTailBuffer,
    wall_time: Duration,
    exit_code: Option<i32>,
    terminal: bool,
}

async fn collect(
    runtime: &dyn ProcessRuntime,
    context: &CodexExecToolContext<'_>,
    session: &CodexExecSession,
    interaction: &mut crate::session::InteractionState,
    yield_time_ms: u64,
) -> Result<CollectedOutput, CodexExecToolError> {
    let mut events = runtime
        .attach(
            context.operation(),
            AttachExecutionRequest {
                execution_id: session.execution_id().clone(),
                after_sequence: Some(interaction.last_sequence),
            },
        )
        .await?;
    let start = tokio::time::Instant::now();
    let deadline = start + Duration::from_millis(yield_time_ms);
    let mut buffer = HeadTailBuffer::default();
    let mut terminal_drain_deadline = None;

    loop {
        let wait_until = terminal_drain_deadline.unwrap_or(deadline);
        let deadline_wait = tokio::time::sleep_until(wait_until);
        tokio::pin!(deadline_wait);
        tokio::select! {
            () = context.operation().cancelled() => {
                return Err(CodexExecToolError::cancelled("unified exec interaction was cancelled"));
            }
            () = &mut deadline_wait => {
                if terminal_drain_deadline.is_some() {
                    return finish_from_inspect(runtime, context, session, interaction, buffer, start).await;
                }
                let status = runtime.inspect(
                    context.operation(),
                    InspectExecutionRequest { execution_id: session.execution_id().clone() },
                ).await?;
                if matches!(status.state, ExecutionState::Running | ExecutionState::Queued | ExecutionState::Starting) {
                    return Ok(CollectedOutput {
                        buffer,
                        wall_time: start.elapsed(),
                        exit_code: status.exit_code,
                        terminal: false,
                    });
                }
                // A terminal status is published only after output readers finish.
                // Give the replay stream a bounded grace period to deliver those
                // already-recorded events rather than dropping final output.
                terminal_drain_deadline = Some(tokio::time::Instant::now() + Duration::from_secs(5));
            }
            event = events.next() => {
                let Some(event) = event else {
                    return finish_from_inspect(runtime, context, session, interaction, buffer, start).await;
                };
                let event = event?;
                interaction.last_sequence = interaction.last_sequence.max(event.sequence);
                match event.event {
                    ProcessEventKind::Started => {}
                    ProcessEventKind::Output { data, .. } => {
                        let bytes = STANDARD.decode(data.0).map_err(|error| {
                            CodexExecToolError::process(format!("process returned invalid base64 output: {error}"))
                        })?;
                        buffer.push(&bytes);
                    }
                    ProcessEventKind::Exited { exit_code, .. } => {
                        return Ok(CollectedOutput {
                            buffer,
                            wall_time: start.elapsed(),
                            exit_code: Some(exit_code),
                            terminal: true,
                        });
                    }
                    ProcessEventKind::Failed { message } => {
                        return Err(CodexExecToolError::process(message));
                    }
                    ProcessEventKind::Closed => {
                        return finish_from_inspect(runtime, context, session, interaction, buffer, start).await;
                    }
                }
            }
        }
    }
}

async fn finish_from_inspect(
    runtime: &dyn ProcessRuntime,
    context: &CodexExecToolContext<'_>,
    session: &CodexExecSession,
    _interaction: &mut crate::session::InteractionState,
    buffer: HeadTailBuffer,
    start: tokio::time::Instant,
) -> Result<CollectedOutput, CodexExecToolError> {
    let status = runtime
        .inspect(
            context.operation(),
            InspectExecutionRequest {
                execution_id: session.execution_id().clone(),
            },
        )
        .await?;
    match status.state {
        ExecutionState::Exited => Ok(CollectedOutput {
            buffer,
            wall_time: start.elapsed(),
            exit_code: status.exit_code,
            terminal: true,
        }),
        ExecutionState::Running | ExecutionState::Queued | ExecutionState::Starting => {
            Ok(CollectedOutput {
                buffer,
                wall_time: start.elapsed(),
                exit_code: status.exit_code,
                terminal: false,
            })
        }
        ExecutionState::Failed | ExecutionState::Cancelled | ExecutionState::Lost => Err(
            CodexExecToolError::process(format!("process ended in state {:?}", status.state)),
        ),
    }
}

async fn finish_output(
    context: &CodexExecToolContext<'_>,
    session: Arc<CodexExecSession>,
    collected: CollectedOutput,
    max_output_tokens: Option<usize>,
) -> Result<CodexExecToolOutput, CodexExecToolError> {
    if collected.terminal {
        remove_session(context, &session).await?;
    }
    let total_bytes = collected.buffer.total_bytes();
    let omitted_bytes = collected.buffer.omitted_bytes();
    let raw_output = collected.buffer.into_bytes();
    let original_token_count = approx_token_count_from_bytes(total_bytes);
    let chunk_id = Uuid::now_v7()
        .simple()
        .to_string()
        .chars()
        .take(6)
        .collect::<String>();
    let response_output = model_output(
        &raw_output,
        max_output_tokens.unwrap_or(DEFAULT_MAX_OUTPUT_TOKENS),
        original_token_count,
        omitted_bytes,
    );
    let visible_session_id = (!collected.terminal).then_some(session.session_id());
    let response = UnifiedExecResult {
        chunk_id: Some(chunk_id.clone()),
        wall_time_seconds: collected.wall_time.as_secs_f64(),
        exit_code: collected.exit_code.filter(|_| collected.terminal),
        session_id: visible_session_id,
        original_token_count: Some(original_token_count),
        output: match max_output_tokens {
            Some(max_tokens) => {
                model_output(&raw_output, max_tokens, original_token_count, omitted_bytes)
            }
            None => String::from_utf8_lossy(&raw_output).into_owned(),
        },
    };
    let mut header = format!(
        "Chunk ID: {chunk_id}\nWall time: {:.4} seconds\n",
        collected.wall_time.as_secs_f64()
    );
    if let Some(exit_code) = response.exit_code {
        header.push_str(&format!("Process exited with code {exit_code}\n"));
    }
    if let Some(session_id) = response.session_id {
        header.push_str(&format!("Process running with session ID {session_id}\n"));
    }
    header.push_str(&format!(
        "Original token count: {original_token_count}\nOutput:\n{response_output}"
    ));
    let details = serde_json::to_value(&response).map_err(|error| {
        CodexExecToolError::process(format!("failed to serialize exec result: {error}"))
    })?;
    Ok(CodexExecToolOutput::new(header, details))
}

#[derive(Serialize)]
struct UnifiedExecResult {
    #[serde(skip_serializing_if = "Option::is_none")]
    chunk_id: Option<String>,
    wall_time_seconds: f64,
    #[serde(skip_serializing_if = "Option::is_none")]
    exit_code: Option<i32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    session_id: Option<i32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    original_token_count: Option<usize>,
    output: String,
}

async fn remove_session(
    context: &CodexExecToolContext<'_>,
    session: &CodexExecSession,
) -> Result<(), CodexExecToolError> {
    context
        .sessions()
        .remove(session.session_id(), session.execution_id())
        .await?;
    Ok(())
}

async fn inspect_is_terminal(
    runtime: &dyn ProcessRuntime,
    context: &CodexExecToolContext<'_>,
    session: &CodexExecSession,
) -> Result<bool, CodexExecToolError> {
    let status = runtime
        .inspect(
            context.operation(),
            InspectExecutionRequest {
                execution_id: session.execution_id().clone(),
            },
        )
        .await?;
    Ok(is_terminal_state(status.state))
}

async fn cleanup_terminal_session(
    runtime: &dyn ProcessRuntime,
    context: &CodexExecToolContext<'_>,
    session: &CodexExecSession,
) {
    if inspect_is_terminal(runtime, context, session)
        .await
        .unwrap_or(false)
    {
        let _ = remove_session(context, session).await;
    }
}

fn is_terminal_state(state: ExecutionState) -> bool {
    matches!(
        state,
        ExecutionState::Exited
            | ExecutionState::Failed
            | ExecutionState::Cancelled
            | ExecutionState::Lost
    )
}

fn clamp_initial_yield(value: u64) -> u64 {
    value.clamp(MIN_YIELD_TIME_MS, MAX_YIELD_TIME_MS)
}

fn clamp_write_yield(value: u64, empty: bool, max_empty: u64) -> u64 {
    let value = value.max(MIN_YIELD_TIME_MS);
    if empty {
        value.clamp(
            MIN_EMPTY_POLL_YIELD_TIME_MS,
            max_empty.max(MIN_EMPTY_POLL_YIELD_TIME_MS),
        )
    } else {
        value.min(MAX_YIELD_TIME_MS)
    }
}

fn operation_id(prefix: &str, tool_call_id: &str) -> OperationId {
    OperationId::new(format!("{prefix}-{tool_call_id}"))
        .expect("non-empty tool call ID produces a valid operation identifier")
}

fn parse<T: DeserializeOwned>(arguments: &ToolArguments) -> Result<T, serde_json::Error> {
    match arguments {
        ToolArguments::Object(arguments) => {
            serde_json::from_value(Value::Object(arguments.clone()))
        }
        ToolArguments::String(arguments) => serde_json::from_str(arguments),
    }
}
