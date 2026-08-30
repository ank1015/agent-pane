//! Reusable implementation of Pi's `read` tool.
//!
//! The crate owns the model-facing tool definition and the execution logic. A
//! harness remains responsible for selecting the tool and translating
//! [`ReadToolOutput`] or [`ReadToolError`] into its transcript representation.

use base64::{Engine as _, engine::general_purpose::STANDARD};
use execution_contracts::{
    ArtifactId, MachineDescriptor, OpenArtifactRequest, PathConvention, PathSpec, ReadMode,
    ReadRequest, ReadResult, TextPageRequest, WorkspaceRootId,
};
use execution_runtime::{ExecutionRuntime, OperationContext};
use futures_util::StreamExt as _;
use llm_contracts::{
    Base64ImageSource, ContentPart, FunctionTool, ImageContent, ImageSource, TextContent,
    ToolArguments, ToolDefinition,
};
use serde::{Deserialize, Serialize, de::DeserializeOwned};
use serde_json::{Value, json};
use url::Url;

/// Name used in the model-facing tool definition.
pub const TOOL_NAME: &str = "read";
/// Maximum number of text lines returned by one read.
pub const DEFAULT_MAX_LINES: usize = 2_000;
/// Maximum number of text bytes returned by one read.
pub const DEFAULT_MAX_BYTES: usize = 50 * 1_024;

const ARTIFACT_CHUNK_BYTES: u64 = 1_024 * 1_024;

/// Typed arguments accepted by the read tool.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ReadArguments {
    pub path: String,
    pub offset: Option<u64>,
    pub limit: Option<u32>,
}

/// Execution inputs specific to the read tool.
pub struct ReadToolContext<'a> {
    runtime: &'a dyn ExecutionRuntime,
    operation: &'a OperationContext,
    workspace_root_id: WorkspaceRootId,
    cwd: String,
}

impl<'a> ReadToolContext<'a> {
    /// Creates a read context rooted at `workspace_root_id` and `cwd`.
    pub fn new(
        runtime: &'a dyn ExecutionRuntime,
        operation: &'a OperationContext,
        workspace_root_id: WorkspaceRootId,
        cwd: impl Into<String>,
    ) -> Result<Self, ReadToolError> {
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

    fn resolve_path(&self, input: &str) -> Result<PathSpec, ReadToolError> {
        resolve_path(
            self.runtime.descriptor(),
            &self.workspace_root_id,
            &self.cwd,
            input,
        )
    }
}

/// Successful output from the read tool.
#[derive(Clone, Debug, PartialEq)]
pub struct ReadToolOutput {
    pub content: Vec<ContentPart>,
    pub details: Option<Value>,
}

impl ReadToolOutput {
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

/// Structured read failure that a harness can map into its own tool result.
#[derive(Debug, thiserror::Error)]
#[error("{message}")]
pub struct ReadToolError {
    name: &'static str,
    message: String,
    details: Option<Value>,
}

impl ReadToolError {
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

impl From<execution_contracts::ExecutionError> for ReadToolError {
    fn from(error: execution_contracts::ExecutionError) -> Self {
        let details = serde_json::to_value(&error).ok();
        Self {
            name: "execution_error",
            message: error.message,
            details,
        }
    }
}

/// Returns the model-facing Pi read tool definition.
#[must_use]
pub fn definition() -> ToolDefinition {
    function_tool(
        TOOL_NAME,
        "Read the contents of a file in the current workspace. Supports text files and images (jpg, png, gif, webp, bmp). Images are sent as attachments. For text files, output is truncated to 2000 lines or 50KB (whichever is hit first). Use offset/limit for large files. When you need the full file, continue with offset until complete.",
        json!({
            "type": "object",
            "properties": {
                "path": {
                    "type": "string",
                    "description": "Path to the file to read (relative or absolute within the current workspace)"
                },
                "offset": {
                    "type": "number",
                    "description": "Line number to start reading from (1-indexed)"
                },
                "limit": {
                    "type": "number",
                    "description": "Maximum number of lines to read"
                }
            },
            "required": ["path"]
        }),
    )
}

/// Parses provider-neutral LLM tool arguments into typed read arguments.
pub fn parse_arguments(arguments: &ToolArguments) -> Result<ReadArguments, ReadToolError> {
    parse(arguments).map_err(ReadToolError::invalid_arguments)
}

/// Parses and executes an LLM tool call's arguments.
pub async fn execute_read_tool(
    arguments: &ToolArguments,
    context: &ReadToolContext<'_>,
) -> Result<ReadToolOutput, ReadToolError> {
    execute(parse_arguments(arguments)?, context).await
}

/// Executes a typed read request against an execution runtime.
pub async fn execute(
    arguments: ReadArguments,
    context: &ReadToolContext<'_>,
) -> Result<ReadToolOutput, ReadToolError> {
    validate_arguments(&arguments)?;
    let path = context.resolve_path(&arguments.path)?;
    let start_line = arguments.offset.unwrap_or(1);
    let max_lines = arguments
        .limit
        .unwrap_or(DEFAULT_MAX_LINES as u32)
        .min(DEFAULT_MAX_LINES as u32);
    let result = context
        .runtime
        .workspace_query()
        .read(
            context.operation,
            ReadRequest {
                path,
                mode: ReadMode::Auto,
                page: Some(TextPageRequest {
                    start_line: Some(start_line),
                    cursor: None,
                    max_lines,
                    max_bytes: DEFAULT_MAX_BYTES as u64,
                    max_line_bytes: DEFAULT_MAX_BYTES as u64,
                    include_total_lines: true,
                }),
            },
        )
        .await?;

    match result {
        ReadResult::Text { page } => {
            let total_lines = page.total_lines.unwrap_or(page.end_line);
            if arguments.offset.is_some() && start_line > total_lines.max(1) {
                return Err(ReadToolError::invalid_arguments(format!(
                    "offset {start_line} is beyond end of file ({total_lines} lines total)"
                )));
            }

            let mut output = page.content;
            if page.lines_truncated {
                output.push_str(&format!(
                    "\n\n[One or more lines exceeded the {} per-line limit.]",
                    format_size(DEFAULT_MAX_BYTES)
                ));
            }
            if page.has_more {
                let next_line = page.next_line.unwrap_or(page.end_line.saturating_add(1));
                let user_limit_stopped_early = arguments
                    .limit
                    .is_some_and(|limit| limit <= DEFAULT_MAX_LINES as u32);
                if user_limit_stopped_early {
                    let remaining = total_lines.saturating_sub(page.end_line);
                    output.push_str(&format!(
                        "\n\n[{remaining} more lines in file. Use offset={next_line} to continue.]"
                    ));
                } else {
                    output.push_str(&format!(
                        "\n\n[Showing lines {}-{} of {total_lines} ({} limit). Use offset={next_line} to continue.]",
                        page.start_line,
                        page.end_line,
                        format_size(DEFAULT_MAX_BYTES)
                    ));
                }
            }

            let truncated = page.has_more || page.lines_truncated;
            let details = truncated.then(|| {
                json!({
                    "truncated": true,
                    "start_line": page.start_line,
                    "end_line": page.end_line,
                    "total_lines": total_lines,
                    "next_line": page.next_line,
                    "lines_truncated": page.lines_truncated,
                    "max_lines": max_lines,
                    "max_bytes": DEFAULT_MAX_BYTES
                })
            });
            let output = ReadToolOutput::text(output);
            Ok(match details {
                Some(details) => output.with_details(details),
                None => output,
            })
        }
        ReadResult::Media { artifact } if is_supported_image(&artifact.mime_type) => {
            let bytes = read_artifact(context, artifact.artifact_id.clone()).await?;
            Ok(ReadToolOutput {
                content: vec![
                    ContentPart::Text(TextContent {
                        content: format!("Read image file [{}]", artifact.mime_type),
                        metadata: None,
                    }),
                    ContentPart::Image(ImageContent {
                        source: ImageSource::Base64(Base64ImageSource {
                            data: STANDARD.encode(bytes),
                            mime_type: artifact.mime_type,
                        }),
                        detail: None,
                        metadata: None,
                    }),
                ],
                details: Some(json!({
                    "artifact_id": artifact.artifact_id,
                    "size": artifact.metadata.size
                })),
            })
        }
        ReadResult::Media { artifact } => Err(ReadToolError::process(format!(
            "read does not support media type `{}`",
            artifact.mime_type
        ))),
        ReadResult::Binary { artifact } => Err(ReadToolError::process(format!(
            "cannot read binary file `{}` as text",
            arguments.path
        ))
        .with_details(json!({
            "artifact_id": artifact.artifact_id,
            "mime_type": artifact.mime_type,
            "size": artifact.metadata.size
        }))),
    }
}

fn validate_arguments(arguments: &ReadArguments) -> Result<(), ReadToolError> {
    if arguments.offset == Some(0) {
        return Err(ReadToolError::invalid_arguments("offset must be one-based"));
    }
    if arguments.limit == Some(0) {
        return Err(ReadToolError::invalid_arguments(
            "limit must be greater than zero",
        ));
    }
    Ok(())
}

async fn read_artifact(
    context: &ReadToolContext<'_>,
    artifact_id: ArtifactId,
) -> Result<Vec<u8>, ReadToolError> {
    let store = context
        .runtime
        .artifact_store()
        .ok_or_else(|| ReadToolError::missing_capability("artifact reads"))?;
    let mut chunks = store
        .open(
            context.operation,
            OpenArtifactRequest {
                artifact_id,
                offset: 0,
                max_bytes: ARTIFACT_CHUNK_BYTES,
            },
        )
        .await?;
    let mut bytes = Vec::new();
    let mut expected_offset = 0_u64;
    loop {
        let chunk = tokio::select! {
            () = context.operation.cancelled() => {
                return Err(ReadToolError::cancelled("image read was cancelled"));
            }
            chunk = chunks.next() => chunk,
        };
        let Some(chunk) = chunk else {
            break;
        };
        let chunk = chunk?;
        if chunk.offset != expected_offset {
            return Err(ReadToolError::process(format!(
                "artifact stream was out of order: expected offset {expected_offset}, received {}",
                chunk.offset
            )));
        }
        let chunk_bytes = STANDARD.decode(chunk.data.0).map_err(|error| {
            ReadToolError::process(format!("artifact contained invalid base64 data: {error}"))
        })?;
        expected_offset = expected_offset.saturating_add(chunk_bytes.len() as u64);
        bytes.extend_from_slice(&chunk_bytes);
        if chunk.eof {
            break;
        }
    }
    Ok(bytes)
}

fn function_tool(name: &str, description: &str, parameters: Value) -> ToolDefinition {
    let Value::Object(parameters) = parameters else {
        unreachable!("tool parameters are declared as an object")
    };
    ToolDefinition::Function(FunctionTool {
        name: name.to_owned(),
        description: description.to_owned(),
        parameters,
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

fn resolve_path(
    descriptor: &MachineDescriptor,
    workspace_root_id: &WorkspaceRootId,
    cwd: &str,
    input: &str,
) -> Result<PathSpec, ReadToolError> {
    let root = descriptor
        .workspace_roots
        .iter()
        .find(|root| &root.id == workspace_root_id)
        .ok_or_else(|| {
            ReadToolError::invalid_path(format!(
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
    let base = if was_absolute { "." } else { cwd };
    Ok(PathSpec::workspace(
        workspace_root_id.clone(),
        normalize_relative_path(base, &input)?,
    ))
}

fn normalize_relative_path(base: &str, input: &str) -> Result<String, ReadToolError> {
    if input.trim().is_empty() {
        return Err(ReadToolError::invalid_path("path must not be empty"));
    }
    if input.contains('\0') {
        return Err(ReadToolError::invalid_path(
            "path must not contain a null byte",
        ));
    }
    if input.starts_with('/') || is_windows_absolute(input) {
        return Err(ReadToolError::invalid_path(
            "absolute path could not be resolved inside the active workspace root",
        ));
    }

    let mut segments = Vec::new();
    for segment in base.split('/').chain(input.split('/')) {
        match segment {
            "" | "." => {}
            ".." => {
                if segments.pop().is_none() {
                    return Err(ReadToolError::invalid_path(
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
) -> Result<String, ReadToolError> {
    let root_url = Url::parse(root_uri)
        .map_err(|_| ReadToolError::invalid_path("workspace root has an invalid absolute URI"))?;
    if root_url.scheme() != "file" {
        return Err(ReadToolError::invalid_path(
            "absolute tool paths require a file-backed workspace root",
        ));
    }

    let mut root = if windows {
        root_url.path().to_owned()
    } else {
        root_url
            .to_file_path()
            .map_err(|()| {
                ReadToolError::invalid_path(
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
        return Err(ReadToolError::invalid_path(
            "absolute path is outside the active workspace root",
        ));
    }
    Ok(input[prefix.len()..].to_owned())
}

fn is_absolute_for(convention: PathConvention, path: &str) -> bool {
    match convention {
        PathConvention::Posix => path.as_bytes().first().is_some_and(|value| *value == b'/'),
        PathConvention::Windows => is_windows_absolute(path),
    }
}

fn is_windows_absolute(path: &str) -> bool {
    let bytes = path.as_bytes();
    (bytes.len() >= 3 && bytes[1] == b':' && (bytes[2] == b'/' || bytes[2] == b'\\'))
        || (bytes.len() >= 2
            && (bytes[0] == b'/' || bytes[0] == b'\\')
            && (bytes[1] == b'/' || bytes[1] == b'\\'))
}

fn is_supported_image(mime_type: &str) -> bool {
    matches!(
        mime_type,
        "image/jpeg" | "image/png" | "image/gif" | "image/webp" | "image/bmp"
    )
}

fn format_size(bytes: usize) -> String {
    if bytes < 1_024 {
        format!("{bytes}B")
    } else if bytes < 1_024 * 1_024 {
        format!("{:.1}KB", bytes as f64 / 1_024.0)
    } else {
        format!("{:.1}MB", bytes as f64 / (1_024.0 * 1_024.0))
    }
}
