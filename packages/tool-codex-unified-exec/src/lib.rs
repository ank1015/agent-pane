//! Reusable implementation of Codex's `exec_command` and `write_stdin` tools.
//!
//! The execution runtime owns operating-system processes and PTYs. A harness-
//! supplied [`CodexExecSessionStore`] owns only the numeric session mapping;
//! this crate owns definitions, lifecycle rules, event cursors, output
//! collection, and model-facing formatting.

mod execution;
mod output;
mod session;

use execution_contracts::{MachineDescriptor, PathConvention, PathSpec, WorkspaceRootId};
use execution_runtime::{ExecutionRuntime, OperationContext};
use llm_contracts::{ContentPart, FunctionTool, TextContent, ToolDefinition};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use url::Url;

pub use execution::{
    execute_exec_command, execute_exec_command_tool, execute_write_stdin, execute_write_stdin_tool,
    parse_exec_command_arguments, parse_write_stdin_arguments,
};
pub use output::{DEFAULT_MAX_OUTPUT_TOKENS, OUTPUT_COLLECTION_MAX_BYTES};
pub use session::{CodexExecSession, CodexExecSessionStore, CodexExecSessionStoreError};

pub const EXEC_COMMAND_TOOL_NAME: &str = "exec_command";
pub const WRITE_STDIN_TOOL_NAME: &str = "write_stdin";

pub const DEFAULT_EXEC_YIELD_TIME_MS: u64 = 10_000;
pub const DEFAULT_WRITE_STDIN_YIELD_TIME_MS: u64 = 250;
pub const MIN_YIELD_TIME_MS: u64 = 250;
pub const MIN_EMPTY_POLL_YIELD_TIME_MS: u64 = 5_000;
pub const MAX_YIELD_TIME_MS: u64 = 30_000;
pub const DEFAULT_MAX_BACKGROUND_YIELD_TIME_MS: u64 = 300_000;

/// Typed arguments accepted by `exec_command` in the basic Codex harness.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ExecCommandArguments {
    pub cmd: String,
    #[serde(default)]
    pub workdir: Option<String>,
    #[serde(default)]
    pub tty: bool,
    #[serde(default = "default_exec_yield_time_ms")]
    pub yield_time_ms: u64,
    #[serde(default)]
    pub max_output_tokens: Option<usize>,
    #[serde(default)]
    pub shell: Option<String>,
    #[serde(default)]
    pub login: Option<bool>,
}

/// Typed arguments accepted by `write_stdin`.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct WriteStdinArguments {
    pub session_id: i32,
    #[serde(default)]
    pub chars: String,
    #[serde(default = "default_write_stdin_yield_time_ms")]
    pub yield_time_ms: u64,
    #[serde(default)]
    pub max_output_tokens: Option<usize>,
}

/// Harness-selected behavior that is not controlled by model arguments.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct CodexExecOptions {
    pub allow_login_shell: bool,
    pub max_empty_poll_yield_time_ms: u64,
    pub pty_columns: u16,
    pub pty_rows: u16,
}

impl Default for CodexExecOptions {
    fn default() -> Self {
        Self {
            allow_login_shell: true,
            max_empty_poll_yield_time_ms: DEFAULT_MAX_BACKGROUND_YIELD_TIME_MS,
            pty_columns: 80,
            pty_rows: 24,
        }
    }
}

/// Explicit execution inputs shared by both unified-exec tools.
pub struct CodexExecToolContext<'a> {
    runtime: &'a dyn ExecutionRuntime,
    operation: &'a OperationContext,
    sessions: &'a dyn CodexExecSessionStore,
    workspace_root_id: WorkspaceRootId,
    cwd: String,
    tool_call_id: String,
    options: CodexExecOptions,
}

impl<'a> CodexExecToolContext<'a> {
    pub fn new(
        runtime: &'a dyn ExecutionRuntime,
        operation: &'a OperationContext,
        sessions: &'a dyn CodexExecSessionStore,
        workspace_root_id: WorkspaceRootId,
        cwd: impl Into<String>,
        tool_call_id: impl Into<String>,
    ) -> Result<Self, CodexExecToolError> {
        let cwd = normalize_relative_path(".", &cwd.into())?;
        let tool_call_id = tool_call_id.into();
        if tool_call_id.trim().is_empty() {
            return Err(CodexExecToolError::invalid_arguments(
                "tool_call_id must not be empty",
            ));
        }
        Ok(Self {
            runtime,
            operation,
            sessions,
            workspace_root_id,
            cwd,
            tool_call_id,
            options: CodexExecOptions::default(),
        })
    }

    #[must_use]
    pub fn with_options(mut self, options: CodexExecOptions) -> Self {
        self.options = options;
        self
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
    pub fn sessions(&self) -> &dyn CodexExecSessionStore {
        self.sessions
    }

    #[must_use]
    pub const fn workspace_root_id(&self) -> &WorkspaceRootId {
        &self.workspace_root_id
    }

    #[must_use]
    pub fn cwd(&self) -> &str {
        &self.cwd
    }

    #[must_use]
    pub fn tool_call_id(&self) -> &str {
        &self.tool_call_id
    }

    #[must_use]
    pub const fn options(&self) -> CodexExecOptions {
        self.options
    }

    fn resolve_workdir(&self, workdir: Option<&str>) -> Result<PathSpec, CodexExecToolError> {
        match workdir.filter(|value| !value.is_empty()) {
            Some(workdir) => resolve_path(
                self.runtime.descriptor(),
                &self.workspace_root_id,
                &self.cwd,
                workdir,
            ),
            None => Ok(PathSpec::workspace(
                self.workspace_root_id.clone(),
                self.cwd.clone(),
            )),
        }
    }
}

/// Structured output shared by both tools.
#[derive(Clone, Debug, PartialEq)]
pub struct CodexExecToolOutput {
    pub content: Vec<ContentPart>,
    pub details: Option<Value>,
}

impl CodexExecToolOutput {
    pub(crate) fn new(text: String, details: Value) -> Self {
        Self {
            content: vec![ContentPart::Text(TextContent {
                content: text,
                metadata: None,
            })],
            details: Some(details),
        }
    }
}

/// Stable structured error a harness can place in its own tool result.
#[derive(Debug, thiserror::Error)]
#[error("{message}")]
pub struct CodexExecToolError {
    name: &'static str,
    message: String,
    details: Option<Value>,
}

impl CodexExecToolError {
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

    fn invalid_arguments(message: impl std::fmt::Display) -> Self {
        Self::new("invalid_arguments", message.to_string())
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

    fn process(message: impl Into<String>) -> Self {
        Self::new("process_error", message)
    }

    fn cancelled(message: impl Into<String>) -> Self {
        Self::new("cancelled", message)
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

impl From<execution_contracts::ExecutionError> for CodexExecToolError {
    fn from(error: execution_contracts::ExecutionError) -> Self {
        let details = serde_json::to_value(&error).ok();
        let value = Self::new("execution_error", error.message);
        match details {
            Some(details) => value.with_details(details),
            None => value,
        }
    }
}

impl From<CodexExecSessionStoreError> for CodexExecToolError {
    fn from(error: CodexExecSessionStoreError) -> Self {
        Self::new("session_store_error", error.to_string())
    }
}

/// Model-facing Codex `exec_command` definition without permission fields.
#[must_use]
pub fn exec_command_definition() -> ToolDefinition {
    function_tool(
        EXEC_COMMAND_TOOL_NAME,
        "Runs a command in a PTY, returning output or a session ID for ongoing interaction.",
        json!({
            "type": "object",
            "properties": {
                "cmd": {
                    "type": "string",
                    "description": "Shell command to execute."
                },
                "workdir": {
                    "type": "string",
                    "description": "Working directory for the command. Defaults to the turn cwd."
                },
                "tty": {
                    "type": "boolean",
                    "description": "True allocates a PTY for the command; false or omitted uses plain pipes."
                },
                "yield_time_ms": {
                    "type": "number",
                    "description": "Wait before yielding output. Defaults to 10000 ms; effective range is 250-30000 ms."
                },
                "max_output_tokens": {
                    "type": "number",
                    "description": "Output token budget. Defaults to 10000 tokens; larger requests may be capped by policy."
                },
                "shell": {
                    "type": "string",
                    "description": "Shell binary to launch. Defaults to the user's default shell."
                },
                "login": {
                    "type": "boolean",
                    "description": "True runs the shell with -l/-i semantics; false disables them. Defaults to true."
                }
            },
            "required": ["cmd"],
            "additionalProperties": false
        }),
    )
}

/// Model-facing Codex `write_stdin` definition.
#[must_use]
pub fn write_stdin_definition() -> ToolDefinition {
    function_tool(
        WRITE_STDIN_TOOL_NAME,
        "Writes characters to an existing unified exec session and returns recent output.",
        json!({
            "type": "object",
            "properties": {
                "session_id": {
                    "type": "number",
                    "description": "Identifier of the running unified exec session."
                },
                "chars": {
                    "type": "string",
                    "description": "Bytes to write to stdin. Defaults to empty, which polls without writing."
                },
                "yield_time_ms": {
                    "type": "number",
                    "description": "Wait before yielding output. Non-empty writes default to 250 ms and cap at 30000 ms; empty polls wait 5000-300000 ms by default."
                },
                "max_output_tokens": {
                    "type": "number",
                    "description": "Output token budget. Defaults to 10000 tokens; larger requests may be capped by policy."
                }
            },
            "required": ["session_id"],
            "additionalProperties": false
        }),
    )
}

/// Returns both definitions in their usual presentation order.
#[must_use]
pub fn definitions() -> Vec<ToolDefinition> {
    vec![exec_command_definition(), write_stdin_definition()]
}

const fn default_exec_yield_time_ms() -> u64 {
    DEFAULT_EXEC_YIELD_TIME_MS
}

const fn default_write_stdin_yield_time_ms() -> u64 {
    DEFAULT_WRITE_STDIN_YIELD_TIME_MS
}

fn function_tool(name: &str, description: &str, parameters: Value) -> ToolDefinition {
    let Value::Object(parameters) = parameters else {
        unreachable!("tool parameters are declared as an object")
    };
    ToolDefinition::Function(FunctionTool {
        name: name.to_owned(),
        description: description.to_owned(),
        parameters,
        output_schema: Some(
            json!({
                "type": "object",
                "properties": {
                    "chunk_id": {
                        "type": "string",
                        "description": "Chunk identifier included when the response reports one."
                    },
                    "wall_time_seconds": {
                        "type": "number",
                        "description": "Elapsed wall time spent waiting for output in seconds."
                    },
                    "exit_code": {
                        "type": "number",
                        "description": "Process exit code when the command finished during this call."
                    },
                    "session_id": {
                        "type": "number",
                        "description": "Session identifier to pass to write_stdin when the process is still running."
                    },
                    "original_token_count": {
                        "type": "number",
                        "description": "Approximate token count before output truncation."
                    },
                    "output": {
                        "type": "string",
                        "description": "Command output text, possibly truncated."
                    }
                },
                "required": ["wall_time_seconds", "output"],
                "additionalProperties": false
            })
            .as_object()
            .expect("output schema is an object")
            .clone(),
        ),
        strict: Some(false),
    })
}

fn resolve_path(
    descriptor: &MachineDescriptor,
    workspace_root_id: &WorkspaceRootId,
    base: &str,
    input: &str,
) -> Result<PathSpec, CodexExecToolError> {
    let root = descriptor
        .workspace_roots
        .iter()
        .find(|root| &root.id == workspace_root_id)
        .ok_or_else(|| {
            CodexExecToolError::invalid_path(format!(
                "workspace root `{workspace_root_id}` is not exposed by the execution runtime"
            ))
        })?;
    let was_absolute = is_absolute_for(descriptor.path_convention, input);
    let input = match descriptor.path_convention {
        PathConvention::Posix if was_absolute => {
            absolute_path_within_root(input, &root.uri, false)?
        }
        PathConvention::Windows if was_absolute => {
            absolute_path_within_root(input, &root.uri, true)?
        }
        PathConvention::Windows => input.replace('\\', "/"),
        PathConvention::Posix => input.to_owned(),
    };
    let base = if was_absolute { "." } else { base };
    Ok(PathSpec::workspace(
        workspace_root_id.clone(),
        normalize_relative_path(base, &input)?,
    ))
}

fn normalize_relative_path(base: &str, input: &str) -> Result<String, CodexExecToolError> {
    if input.trim().is_empty() {
        return Err(CodexExecToolError::invalid_path("path must not be empty"));
    }
    if input.contains('\0') {
        return Err(CodexExecToolError::invalid_path(
            "path must not contain a null byte",
        ));
    }
    if input.starts_with('/') || is_windows_absolute(input) {
        return Err(CodexExecToolError::invalid_path(
            "absolute path could not be resolved inside the active workspace root",
        ));
    }
    let mut segments = Vec::new();
    for segment in base.split('/').chain(input.split('/')) {
        match segment {
            "" | "." => {}
            ".." => {
                if segments.pop().is_none() {
                    return Err(CodexExecToolError::invalid_path(
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

fn absolute_path_within_root(
    input: &str,
    root_uri: &str,
    windows: bool,
) -> Result<String, CodexExecToolError> {
    let root_url = Url::parse(root_uri).map_err(|_| {
        CodexExecToolError::invalid_path("workspace root has an invalid absolute URI")
    })?;
    if root_url.scheme() != "file" {
        return Err(CodexExecToolError::invalid_path(
            "absolute tool paths require a file-backed workspace root",
        ));
    }
    let mut root = if windows {
        root_url.path().to_owned()
    } else {
        root_url
            .to_file_path()
            .map_err(|()| {
                CodexExecToolError::invalid_path(
                    "workspace root file URI cannot be converted to an absolute path",
                )
            })?
            .to_string_lossy()
            .into_owned()
    };
    root.truncate(root.trim_end_matches('/').len());
    let mut input = input.replace('\\', "/");
    if windows {
        root = root.trim_start_matches('/').to_owned();
        input = input.trim_start_matches('/').to_owned();
    }
    let matches_root = if windows {
        input.eq_ignore_ascii_case(&root)
    } else {
        input == root
    };
    if matches_root {
        return Ok(".".to_owned());
    }
    let prefix = format!("{root}/");
    let within_root = if windows {
        input
            .get(..prefix.len())
            .is_some_and(|value| value.eq_ignore_ascii_case(&prefix))
    } else {
        input.starts_with(&prefix)
    };
    if !within_root {
        return Err(CodexExecToolError::invalid_path(
            "absolute path is outside the active workspace root",
        ));
    }
    Ok(input[prefix.len()..].to_owned())
}

fn is_absolute_for(convention: PathConvention, path: &str) -> bool {
    match convention {
        PathConvention::Posix => path.starts_with('/'),
        PathConvention::Windows => is_windows_absolute(path),
    }
}

fn is_windows_absolute(path: &str) -> bool {
    let bytes = path.as_bytes();
    (bytes.len() >= 3 && bytes[1] == b':' && matches!(bytes[2], b'/' | b'\\'))
        || (bytes.len() >= 2
            && matches!(bytes[0], b'/' | b'\\')
            && matches!(bytes[1], b'/' | b'\\'))
}
