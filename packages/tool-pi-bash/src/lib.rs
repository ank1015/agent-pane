//! Reusable implementation of Pi's `bash` tool.
//!
//! The crate owns the model-facing definition and process lifecycle, including
//! timeout, cancellation, environment filtering, output truncation, and full
//! output artifact metadata. Harnesses remain responsible for selecting the
//! tool and recording its result.

mod truncate;

use std::{future::pending, time::Duration};

use base64::{Engine as _, engine::general_purpose::STANDARD};
use execution_contracts::{
    AttachExecutionRequest, CommandSpec, EnvironmentInheritance, EnvironmentVariables, ExecutionId,
    ExecutionPersistence, ExecutionPolicy, ExecutionState, InspectExecutionRequest, NetworkMode,
    OperationId, PathSpec, ProcessEventKind, ProcessOutputPolicy, SandboxMode,
    StartExecutionRequest, StdinMode, TerminateExecutionRequest, WorkspaceRootId,
};
use execution_runtime::{ExecutionRuntime, OperationContext};
use futures_util::StreamExt as _;
use llm_contracts::{ContentPart, FunctionTool, TextContent, ToolArguments, ToolDefinition};
use serde::{Deserialize, Serialize, de::DeserializeOwned};
use serde_json::{Value, json};
use uuid::Uuid;

use truncate::{DEFAULT_MAX_BYTES, TruncatedBy, format_size, truncate_tail};

/// Name used in the model-facing tool definition.
pub const TOOL_NAME: &str = "bash";
const MAX_TIMEOUT_MS: f64 = 2_147_483_647.0;
const OUTPUT_CHUNK_BYTES: u32 = 16 * 1_024;

/// Typed arguments accepted by the Bash tool.
#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct BashArguments {
    pub command: String,
    pub timeout: Option<f64>,
}

/// Execution inputs specific to the Bash tool.
pub struct BashToolContext<'a> {
    runtime: &'a dyn ExecutionRuntime,
    operation: &'a OperationContext,
    workspace_root_id: WorkspaceRootId,
    cwd: String,
}

impl<'a> BashToolContext<'a> {
    /// Creates a Bash context rooted at `workspace_root_id` and `cwd`.
    pub fn new(
        runtime: &'a dyn ExecutionRuntime,
        operation: &'a OperationContext,
        workspace_root_id: WorkspaceRootId,
        cwd: impl Into<String>,
    ) -> Result<Self, BashToolError> {
        let cwd = normalize_relative_path(".", &cwd.into())?;
        Ok(Self {
            runtime,
            operation,
            workspace_root_id,
            cwd,
        })
    }

    #[must_use]
    pub fn runtime(&self) -> &dyn ExecutionRuntime {
        self.runtime
    }

    #[must_use]
    pub const fn operation(&self) -> &OperationContext {
        self.operation
    }

    #[must_use]
    pub const fn workspace_root_id(&self) -> &WorkspaceRootId {
        &self.workspace_root_id
    }

    #[must_use]
    pub fn cwd(&self) -> &str {
        &self.cwd
    }

    fn cwd_path(&self) -> Result<PathSpec, BashToolError> {
        let exposed = self
            .runtime
            .descriptor()
            .workspace_roots
            .iter()
            .any(|root| root.id == self.workspace_root_id);
        if !exposed {
            return Err(BashToolError::invalid_path(format!(
                "workspace root `{}` is not exposed by the execution runtime",
                self.workspace_root_id
            )));
        }
        Ok(PathSpec::workspace(
            self.workspace_root_id.clone(),
            self.cwd.clone(),
        ))
    }
}

/// Successful output from the Bash tool.
#[derive(Clone, Debug, PartialEq)]
pub struct BashToolOutput {
    pub content: Vec<ContentPart>,
    pub details: Option<Value>,
}

impl BashToolOutput {
    fn text(content: impl Into<String>) -> Self {
        Self {
            content: vec![ContentPart::Text(TextContent {
                content: content.into(),
                metadata: None,
            })],
            details: None,
        }
    }

    fn with_details(mut self, details: Value) -> Self {
        self.details = Some(details);
        self
    }
}

/// Structured Bash failure that a harness can map into its own tool result.
#[derive(Debug, thiserror::Error)]
#[error("{message}")]
pub struct BashToolError {
    name: &'static str,
    message: String,
    details: Option<Value>,
}

impl BashToolError {
    fn new(name: &'static str, message: impl Into<String>) -> Self {
        Self {
            name,
            message: message.into(),
            details: None,
        }
    }

    fn with_details(mut self, details: Value) -> Self {
        self.details = Some(details);
        self
    }

    fn with_optional_details(self, details: Option<Value>) -> Self {
        match details {
            Some(details) => self.with_details(details),
            None => self,
        }
    }

    fn invalid_arguments(message: impl std::fmt::Display) -> Self {
        Self::new(
            "invalid_arguments",
            format!("Invalid arguments for {TOOL_NAME}: {message}"),
        )
    }

    fn invalid_path(message: impl Into<String>) -> Self {
        Self::new("invalid_path", message)
    }

    fn missing_capability(capability: &str) -> Self {
        Self::new(
            "unsupported_capability",
            format!("Execution runtime does not support {capability}"),
        )
    }

    fn command(message: impl Into<String>, details: Option<Value>) -> Self {
        Self::new("command_failed", message).with_optional_details(details)
    }

    fn cancelled(message: impl Into<String>) -> Self {
        Self::new("cancelled", message)
    }

    fn process(message: impl Into<String>) -> Self {
        Self::new("process_error", message)
    }

    #[must_use]
    pub const fn name(&self) -> &'static str {
        self.name
    }

    #[must_use]
    pub fn message(&self) -> &str {
        &self.message
    }

    #[must_use]
    pub const fn details(&self) -> Option<&Value> {
        self.details.as_ref()
    }

    #[must_use]
    pub fn into_parts(self) -> (&'static str, String, Option<Value>) {
        (self.name, self.message, self.details)
    }
}

impl From<execution_contracts::ExecutionError> for BashToolError {
    fn from(error: execution_contracts::ExecutionError) -> Self {
        let details = serde_json::to_value(&error).ok();
        Self {
            name: "execution_error",
            message: error.message,
            details,
        }
    }
}

/// Returns the model-facing Pi Bash tool definition.
#[must_use]
pub fn definition() -> ToolDefinition {
    function_tool(
        TOOL_NAME,
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

/// Parses provider-neutral LLM tool arguments into typed Bash arguments.
pub fn parse_arguments(arguments: &ToolArguments) -> Result<BashArguments, BashToolError> {
    parse(arguments).map_err(BashToolError::invalid_arguments)
}

/// Parses and executes an LLM tool call's arguments.
pub async fn execute_bash_tool(
    arguments: &ToolArguments,
    context: &BashToolContext<'_>,
) -> Result<BashToolOutput, BashToolError> {
    execute(parse_arguments(arguments)?, context).await
}

/// Executes a typed shell command against an execution runtime.
pub async fn execute(
    arguments: BashArguments,
    context: &BashToolContext<'_>,
) -> Result<BashToolOutput, BashToolError> {
    if arguments.command.is_empty() {
        return Err(BashToolError::invalid_arguments(
            "command must not be empty",
        ));
    }
    let timeout = parse_timeout(arguments.timeout)?;
    let runtime = context
        .runtime
        .process_runtime()
        .ok_or_else(|| BashToolError::missing_capability("process sessions"))?;
    let execution_id = ExecutionId::new(format!("pi-bash-{}", Uuid::now_v7()))
        .expect("UUID execution identifier is valid");
    let cwd = context.cwd_path()?;

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
            () = context.operation.cancelled() => {
                terminate(runtime, &execution_id).await;
                let (text, details) = format_output(&output, None);
                return Err(BashToolError::cancelled(append_status(text, "Command aborted"))
                    .with_optional_details(details));
            }
            () = &mut timeout_wait => {
                terminate(runtime, &execution_id).await;
                let seconds = arguments.timeout.expect("timeout future only completes when set");
                let (text, details) = format_output(&output, None);
                return Err(BashToolError::command(
                    append_status(text, &format!("Command timed out after {seconds} seconds")),
                    details,
                ));
            }
            event = events.next() => {
                match event {
                    Some(Ok(event)) => match event.event {
                        ProcessEventKind::Output { data, .. } => {
                            let bytes = STANDARD.decode(data.0).map_err(|error| {
                                BashToolError::process(format!("process returned invalid base64 output: {error}"))
                            })?;
                            output.extend_from_slice(&bytes);
                        }
                        ProcessEventKind::Exited { exit_code, sandbox_denied } => {
                            if sandbox_denied {
                                let (text, details) = format_output(&output, None);
                                return Err(BashToolError::command(
                                    append_status(text, "Command was denied by the execution sandbox"),
                                    details,
                                ));
                            }
                            break exit_code;
                        }
                        ProcessEventKind::Failed { message } => {
                            let (text, details) = format_output(&output, None);
                            return Err(BashToolError::command(
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
                            return Err(BashToolError::process("process event stream closed before the command exited"));
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
                        return Err(BashToolError::process("process event stream ended before the command exited"));
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
        return Err(BashToolError::command(
            append_status(text, &format!("Command exited with code {exit_code}")),
            details,
        ));
    }

    let output = BashToolOutput::text(text);
    Ok(match details {
        Some(details) => output.with_details(details),
        None => output,
    })
}

fn parse_timeout(timeout: Option<f64>) -> Result<Option<Duration>, BashToolError> {
    let Some(seconds) = timeout else {
        return Ok(None);
    };
    if !seconds.is_finite() || seconds <= 0.0 {
        return Err(BashToolError::invalid_arguments(
            "timeout must be a finite positive number of seconds",
        ));
    }
    if seconds * 1_000.0 > MAX_TIMEOUT_MS {
        return Err(BashToolError::invalid_arguments(format!(
            "timeout must not exceed {} seconds",
            MAX_TIMEOUT_MS / 1_000.0
        )));
    }
    Ok(Some(Duration::from_secs_f64(seconds)))
}

fn duration_ms(duration: Duration) -> u64 {
    u64::try_from(duration.as_millis())
        .unwrap_or(u64::MAX)
        .max(1)
}

async fn terminate(runtime: &dyn execution_runtime::ProcessRuntime, execution_id: &ExecutionId) {
    let cleanup = OperationContext::with_timeout(Duration::from_secs(5));
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

fn operation_id(prefix: &str) -> OperationId {
    OperationId::new(format!("{prefix}-{}", Uuid::now_v7()))
        .expect("UUID operation identifier is valid")
}

fn function_tool(name: &str, description: &str, parameters: Value) -> ToolDefinition {
    let Value::Object(parameters) = parameters else {
        unreachable!("tool parameters are declared as an object")
    };
    ToolDefinition::Function(FunctionTool {
        name: name.to_owned(),
        description: description.to_owned(),
        parameters,
        output_schema: None,
        strict: None,
    })
}

fn parse<T: DeserializeOwned>(arguments: &ToolArguments) -> Result<T, serde_json::Error> {
    match arguments {
        ToolArguments::Object(arguments) => {
            serde_json::from_value(Value::Object(arguments.clone()))
        }
        ToolArguments::String(arguments) => serde_json::from_str(arguments),
    }
}

fn normalize_relative_path(base: &str, input: &str) -> Result<String, BashToolError> {
    if input.trim().is_empty() {
        return Err(BashToolError::invalid_path("path must not be empty"));
    }
    if input.contains('\0') {
        return Err(BashToolError::invalid_path(
            "path must not contain a null byte",
        ));
    }
    if input.starts_with('/') || is_windows_absolute(input) {
        return Err(BashToolError::invalid_path(
            "absolute path could not be resolved inside the active workspace root",
        ));
    }

    let mut segments = Vec::new();
    for segment in base.split('/').chain(input.split('/')) {
        match segment {
            "" | "." => {}
            ".." => {
                if segments.pop().is_none() {
                    return Err(BashToolError::invalid_path(
                        "path traverses above the active workspace root",
                    ));
                }
            }
            value => segments.push(value),
        }
    }
    Ok(if segments.is_empty() {
        ".".to_owned()
    } else {
        segments.join("/")
    })
}

fn is_windows_absolute(path: &str) -> bool {
    let bytes = path.as_bytes();
    (bytes.len() >= 3 && bytes[1] == b':' && (bytes[2] == b'/' || bytes[2] == b'\\'))
        || (bytes.len() >= 2
            && (bytes[0] == b'/' || bytes[0] == b'\\')
            && (bytes[1] == b'/' || bytes[1] == b'\\'))
}
