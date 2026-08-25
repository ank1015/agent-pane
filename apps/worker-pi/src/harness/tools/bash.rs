use std::{future::pending, time::Duration};

use base64::{Engine, engine::general_purpose::STANDARD};
use execution_contracts::{
    AttachExecutionRequest, CommandSpec, EnvironmentInheritance, EnvironmentVariables, ExecutionId,
    ExecutionPersistence, ExecutionPolicy, ExecutionState, InspectExecutionRequest, NetworkMode,
    ProcessEventKind, ProcessOutputPolicy, SandboxMode, StartExecutionRequest, StdinMode,
    TerminateExecutionRequest,
};
use futures_util::StreamExt;
use llm_contracts::{ToolArguments, ToolDefinition};
use serde::Deserialize;
use serde_json::{Value, json};
use uuid::Uuid;

use super::{
    ToolExecutionContext, ToolExecutionError, ToolOutput, function_tool, operation_id,
    parse_arguments, resolve_path,
    truncate::{DEFAULT_MAX_BYTES, TruncatedBy, format_size, truncate_tail},
};

const MAX_TIMEOUT_MS: f64 = 2_147_483_647.0;
const OUTPUT_CHUNK_BYTES: u32 = 16 * 1_024;

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct BashArguments {
    command: String,
    timeout: Option<f64>,
}

pub fn definition() -> ToolDefinition {
    function_tool(
        "bash",
        "Execute a bash command in the current working directory. Returns stdout and stderr. Output is truncated to last 2000 lines or 50KB (whichever is hit first). If truncated, full output is retained as an execution artifact. Optionally provide a timeout in seconds.",
        json!({
            "type": "object",
            "properties": {
                "command": {
                    "type": "string",
                    "description": "Bash command to execute"
                },
                "timeout": {
                    "type": "number",
                    "description": "Timeout in seconds (optional, no default timeout)"
                }
            },
            "required": ["command"]
        }),
    )
}

pub async fn execute_bash_tool(
    arguments: &ToolArguments,
    context: &ToolExecutionContext<'_>,
) -> Result<ToolOutput, ToolExecutionError> {
    let arguments: BashArguments = parse_arguments("bash", arguments)?;
    if arguments.command.is_empty() {
        return Err(ToolExecutionError::invalid_arguments(
            "bash",
            "command must not be empty",
        ));
    }
    let timeout = parse_timeout(arguments.timeout)?;
    let runtime = context
        .runtime
        .process_runtime()
        .ok_or_else(|| ToolExecutionError::missing_capability("process sessions"))?;
    let execution_id = ExecutionId::new(format!("pi-bash-{}", Uuid::now_v7()))
        .expect("UUID execution identifier is valid");
    let cwd = resolve_path(context, ".")?;

    if let Err(error) = runtime
        .start(
            context.operation,
            StartExecutionRequest {
                operation_id: operation_id("pi-bash-start"),
                execution_id: execution_id.clone(),
                command: CommandSpec::Shell {
                    command: arguments.command,
                    shell: None,
                    login: false,
                },
                cwd,
                environment: EnvironmentVariables {
                    inherit: EnvironmentInheritance::All,
                    allow: Vec::new(),
                    remove: vec![
                        "PI_SESSION_ID".to_owned(),
                        "PI_SESSION_FILE".to_owned(),
                        "PI_PROVIDER".to_owned(),
                        "PI_MODEL".to_owned(),
                        "PI_REASONING_LEVEL".to_owned(),
                    ],
                    set: Default::default(),
                },
                stdin: StdinMode::Closed,
                timeout_ms: timeout.map(duration_ms),
                persistence: ExecutionPersistence::KeepUntilExit,
                policy: ExecutionPolicy {
                    sandbox: SandboxMode::Disabled,
                    network: NetworkMode::Inherit,
                    profile: None,
                    resource_limits: None,
                },
                output: ProcessOutputPolicy {
                    persist_full_output: true,
                    max_inline_bytes: DEFAULT_MAX_BYTES as u64,
                    max_chunk_bytes: OUTPUT_CHUNK_BYTES,
                },
            },
        )
        .await
    {
        terminate(runtime, &execution_id).await;
        return Err(error.into());
    }

    let mut events = match runtime
        .attach(
            context.operation,
            AttachExecutionRequest {
                execution_id: execution_id.clone(),
                after_sequence: None,
            },
        )
        .await
    {
        Ok(events) => events,
        Err(error) => {
            terminate(runtime, &execution_id).await;
            return Err(error.into());
        }
    };
    let mut output = Vec::new();
    let timeout_wait = async {
        match timeout {
            Some(timeout) => tokio::time::sleep(timeout).await,
            None => pending().await,
        }
    };
    tokio::pin!(timeout_wait);

    let exit_code = loop {
        tokio::select! {
            _ = context.operation.cancelled() => {
                terminate(runtime, &execution_id).await;
                let (text, details) = format_output(&output, None);
                return Err(ToolExecutionError::cancelled(append_status(text, "Command aborted"))
                    .with_optional_details(details));
            }
            () = &mut timeout_wait => {
                terminate(runtime, &execution_id).await;
                let seconds = arguments.timeout.expect("timeout future only completes when set");
                let (text, details) = format_output(&output, None);
                return Err(ToolExecutionError::command(
                    append_status(text, &format!("Command timed out after {seconds} seconds")),
                    details,
                ));
            }
            event = events.next() => {
                match event {
                    Some(Ok(event)) => match event.event {
                        ProcessEventKind::Output { data, .. } => {
                            let bytes = STANDARD.decode(data.0).map_err(|error| {
                                ToolExecutionError::process(format!("process returned invalid base64 output: {error}"))
                            })?;
                            output.extend_from_slice(&bytes);
                        }
                        ProcessEventKind::Exited { exit_code, sandbox_denied } => {
                            if sandbox_denied {
                                let (text, details) = format_output(&output, None);
                                return Err(ToolExecutionError::command(
                                    append_status(text, "Command was denied by the execution sandbox"),
                                    details,
                                ));
                            }
                            break exit_code;
                        }
                        ProcessEventKind::Failed { message } => {
                            let (text, details) = format_output(&output, None);
                            return Err(ToolExecutionError::command(
                                append_status(text, &message),
                                details,
                            ));
                        }
                        ProcessEventKind::Closed => {
                            let status = runtime
                                .inspect(
                                    context.operation,
                                    InspectExecutionRequest { execution_id: execution_id.clone() },
                                )
                                .await?;
                            if status.state == ExecutionState::Exited {
                                break status.exit_code.unwrap_or(-1);
                            }
                            return Err(ToolExecutionError::process("process event stream closed before the command exited"));
                        }
                        ProcessEventKind::Started => {}
                    },
                    Some(Err(error)) => {
                        terminate(runtime, &execution_id).await;
                        return Err(error.into());
                    }
                    None => {
                        let status = runtime
                            .inspect(
                                context.operation,
                                InspectExecutionRequest { execution_id: execution_id.clone() },
                            )
                            .await?;
                        if status.state == ExecutionState::Exited {
                            break status.exit_code.unwrap_or(-1);
                        }
                        terminate(runtime, &execution_id).await;
                        return Err(ToolExecutionError::process("process event stream ended before the command exited"));
                    }
                }
            }
        }
    };

    let artifact = runtime
        .inspect(
            context.operation,
            InspectExecutionRequest {
                execution_id: execution_id.clone(),
            },
        )
        .await
        .ok()
        .and_then(|status| status.full_output_artifact);
    let (text, details) = format_output(&output, artifact.as_ref().map(ToString::to_string));
    if exit_code != 0 {
        return Err(ToolExecutionError::command(
            append_status(text, &format!("Command exited with code {exit_code}")),
            details,
        ));
    }

    let output = ToolOutput::text(text);
    Ok(match details {
        Some(details) => output.with_details(details),
        None => output,
    })
}

fn parse_timeout(timeout: Option<f64>) -> Result<Option<Duration>, ToolExecutionError> {
    let Some(seconds) = timeout else {
        return Ok(None);
    };
    if !seconds.is_finite() || seconds <= 0.0 {
        return Err(ToolExecutionError::invalid_arguments(
            "bash",
            "timeout must be a finite positive number of seconds",
        ));
    }
    if seconds * 1_000.0 > MAX_TIMEOUT_MS {
        return Err(ToolExecutionError::invalid_arguments(
            "bash",
            format!(
                "timeout must not exceed {} seconds",
                MAX_TIMEOUT_MS / 1_000.0
            ),
        ));
    }
    Ok(Some(Duration::from_secs_f64(seconds)))
}

fn duration_ms(duration: Duration) -> u64 {
    u64::try_from(duration.as_millis())
        .unwrap_or(u64::MAX)
        .max(1)
}

async fn terminate(runtime: &dyn execution_runtime::ProcessRuntime, execution_id: &ExecutionId) {
    let cleanup = execution_runtime::OperationContext::with_timeout(Duration::from_secs(5));
    let _ = runtime
        .terminate(
            &cleanup,
            TerminateExecutionRequest {
                execution_id: execution_id.clone(),
            },
        )
        .await;
}

fn format_output(output: &[u8], artifact_id: Option<String>) -> (String, Option<Value>) {
    let output = String::from_utf8_lossy(output);
    let truncation = truncate_tail(&output);
    let mut text = if truncation.content.is_empty() {
        "(no output)".to_owned()
    } else {
        truncation.content.clone()
    };
    if !truncation.truncated {
        return (text, None);
    }

    let start_line = truncation.total_lines - truncation.output_lines + 1;
    let end_line = truncation.total_lines;
    let artifact_note = artifact_id
        .as_deref()
        .map_or(String::new(), |id| format!(" Full output artifact: {id}."));
    if truncation.last_line_partial {
        text.push_str(&format!(
            "\n\n[Showing last {} of line {end_line}.{}]",
            format_size(truncation.output_bytes),
            artifact_note
        ));
    } else if matches!(truncation.truncated_by, Some(TruncatedBy::Lines)) {
        text.push_str(&format!(
            "\n\n[Showing lines {start_line}-{end_line} of {}.{}]",
            truncation.total_lines, artifact_note
        ));
    } else {
        text.push_str(&format!(
            "\n\n[Showing lines {start_line}-{end_line} of {} ({} limit).{}]",
            truncation.total_lines,
            format_size(DEFAULT_MAX_BYTES),
            artifact_note
        ));
    }
    let mut details = serde_json::to_value(&truncation).expect("truncation details serialize");
    if let (Some(id), Value::Object(details)) = (artifact_id, &mut details) {
        details.insert("full_output_artifact_id".to_owned(), json!(id));
    }
    (text, Some(details))
}

fn append_status(text: String, status: &str) -> String {
    if text.is_empty() {
        status.to_owned()
    } else {
        format!("{text}\n\n{status}")
    }
}

trait OptionalDetails {
    fn with_optional_details(self, details: Option<Value>) -> Self;
}

impl OptionalDetails for ToolExecutionError {
    fn with_optional_details(self, details: Option<Value>) -> Self {
        match details {
            Some(details) => self.with_details(details),
            None => self,
        }
    }
}
