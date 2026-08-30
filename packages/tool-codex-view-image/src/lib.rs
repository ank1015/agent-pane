//! Reusable implementation of Codex's `view_image` tool.
//!
//! The crate owns the model-facing tool definition and image loading logic. A
//! harness remains responsible for selecting the tool and translating
//! [`ViewImageToolOutput`] or [`ViewImageToolError`] into its transcript.

use base64::{Engine as _, engine::general_purpose::STANDARD};
use execution_contracts::{
    ArtifactId, MachineDescriptor, OpenArtifactRequest, PathConvention, PathSpec, ReadMode,
    ReadRequest, ReadResult, WorkspaceRootId,
};
use execution_runtime::{ExecutionRuntime, OperationContext};
use futures_util::StreamExt as _;
use image::{GenericImageView as _, ImageFormat};
use llm_contracts::{
    Base64ImageSource, ContentPart, FunctionTool, ImageContent, ImageDetail, ImageSource,
    ToolArguments, ToolDefinition,
};
use serde::{Deserialize, Serialize, de::DeserializeOwned};
use serde_json::{Value, json};
use url::Url;

/// Name used in the model-facing tool definition.
pub const TOOL_NAME: &str = "view_image";

const ARTIFACT_CHUNK_BYTES: u64 = 1_024 * 1_024;
const VIEW_IMAGE_INVALID_MESSAGE: &str =
    "unable to process image: invalid or unsupported image data";

/// Image detail levels accepted by the Codex tool.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ViewImageDetail {
    High,
    Original,
}

impl From<ViewImageDetail> for ImageDetail {
    fn from(value: ViewImageDetail) -> Self {
        match value {
            ViewImageDetail::High => Self::High,
            ViewImageDetail::Original => Self::Original,
        }
    }
}

/// Typed arguments accepted by the view-image tool.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ViewImageArguments {
    pub path: String,
    pub detail: Option<ViewImageDetail>,
}

/// Execution inputs specific to the view-image tool.
pub struct ViewImageToolContext<'a> {
    runtime: &'a dyn ExecutionRuntime,
    operation: &'a OperationContext,
    workspace_root_id: WorkspaceRootId,
    cwd: String,
}

impl<'a> ViewImageToolContext<'a> {
    /// Creates an image context rooted at `workspace_root_id` and `cwd`.
    pub fn new(
        runtime: &'a dyn ExecutionRuntime,
        operation: &'a OperationContext,
        workspace_root_id: WorkspaceRootId,
        cwd: impl Into<String>,
    ) -> Result<Self, ViewImageToolError> {
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

    fn resolve_path(&self, input: &str) -> Result<PathSpec, ViewImageToolError> {
        resolve_path(
            self.runtime.descriptor(),
            &self.workspace_root_id,
            &self.cwd,
            input,
        )
    }
}

/// Successful output from the view-image tool.
#[derive(Clone, Debug, PartialEq)]
pub struct ViewImageToolOutput {
    pub content: Vec<ContentPart>,
    pub details: Option<Value>,
}

/// Structured image failure that a harness can map into its tool result.
#[derive(Debug, thiserror::Error)]
#[error("{message}")]
pub struct ViewImageToolError {
    name: &'static str,
    message: String,
    details: Option<Value>,
}

impl ViewImageToolError {
    fn new(name: &'static str, message: impl Into<String>) -> Self {
        Self {
            name,
            message: message.into(),
            details: None,
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

    fn invalid_image(message: impl Into<String>) -> Self {
        Self::new("invalid_image", message)
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

impl From<execution_contracts::ExecutionError> for ViewImageToolError {
    fn from(error: execution_contracts::ExecutionError) -> Self {
        let details = serde_json::to_value(&error).ok();
        Self {
            name: "execution_error",
            message: error.message,
            details,
        }
    }
}

/// Returns the model-facing Codex view-image tool definition.
#[must_use]
pub fn definition() -> ToolDefinition {
    function_tool(
        TOOL_NAME,
        "View a local image file from the filesystem when visual inspection is needed. Use this for images already available on disk.",
        json!({
            "type": "object",
            "properties": {
                "path": {
                    "type": "string",
                    "description": "Local filesystem path to an image file."
                },
                "detail": {
                    "type": "string",
                    "enum": ["high", "original"],
                    "description": "Image detail level. Defaults to `high`; use `original` to preserve exact resolution."
                }
            },
            "required": ["path"],
            "additionalProperties": false
        }),
    )
}

/// Parses provider-neutral LLM tool arguments into typed view-image arguments.
pub fn parse_arguments(
    arguments: &ToolArguments,
) -> Result<ViewImageArguments, ViewImageToolError> {
    #[derive(Deserialize)]
    #[serde(deny_unknown_fields)]
    struct RawViewImageArguments {
        path: String,
        detail: Option<String>,
    }

    let raw: RawViewImageArguments =
        parse(arguments).map_err(ViewImageToolError::invalid_arguments)?;
    let detail = match raw.detail.as_deref() {
        None => None,
        Some("high") => Some(ViewImageDetail::High),
        Some("original") => Some(ViewImageDetail::Original),
        Some(detail) => {
            return Err(ViewImageToolError::new(
                "invalid_arguments",
                format!(
                    "view_image.detail only supports `high` or `original`; omit `detail` for default high resized behavior, got `{detail}`"
                ),
            ));
        }
    };
    Ok(ViewImageArguments {
        path: raw.path,
        detail,
    })
}

/// Parses and executes an LLM view-image tool call.
pub async fn execute_view_image_tool(
    arguments: &ToolArguments,
    context: &ViewImageToolContext<'_>,
) -> Result<ViewImageToolOutput, ViewImageToolError> {
    execute(parse_arguments(arguments)?, context).await
}

/// Executes a typed image request against an execution runtime.
pub async fn execute(
    arguments: ViewImageArguments,
    context: &ViewImageToolContext<'_>,
) -> Result<ViewImageToolOutput, ViewImageToolError> {
    let path = context.resolve_path(&arguments.path)?;
    let result = context
        .runtime
        .workspace_query()
        .read(
            context.operation,
            ReadRequest {
                path,
                mode: ReadMode::Media,
                page: None,
            },
        )
        .await?;

    let artifact = match result {
        ReadResult::Media { artifact } | ReadResult::Binary { artifact } => artifact,
        ReadResult::Text { .. } => {
            return Err(ViewImageToolError::process(
                "execution runtime returned text for a media read",
            ));
        }
    };
    let bytes = read_artifact(
        context,
        artifact.artifact_id.clone(),
        artifact.metadata.size,
    )
    .await?;
    let format = image::guess_format(&bytes)
        .map_err(|_| ViewImageToolError::invalid_image(VIEW_IMAGE_INVALID_MESSAGE))?;
    let mime_type = supported_mime_type(format)?;
    let decoded = image::load_from_memory_with_format(&bytes, format)
        .map_err(|_| ViewImageToolError::invalid_image(VIEW_IMAGE_INVALID_MESSAGE))?;
    let (width, height) = decoded.dimensions();
    let detail = arguments.detail.unwrap_or(ViewImageDetail::High);
    let provider_detail = ImageDetail::from(detail);

    Ok(ViewImageToolOutput {
        content: vec![ContentPart::Image(ImageContent {
            source: ImageSource::Base64(Base64ImageSource {
                data: STANDARD.encode(&bytes),
                // Match Codex: the tool returns unmodified file bytes and lets
                // centralized request preparation identify/transcode them.
                mime_type: "application/octet-stream".to_owned(),
            }),
            detail: Some(provider_detail),
            metadata: None,
        })],
        details: Some(json!({
            "artifact_id": artifact.artifact_id,
            "path": arguments.path,
            "mime_type": mime_type,
            "size": bytes.len(),
            "width": width,
            "height": height,
            "detail": detail
        })),
    })
}

async fn read_artifact(
    context: &ViewImageToolContext<'_>,
    artifact_id: ArtifactId,
    expected_size: u64,
) -> Result<Vec<u8>, ViewImageToolError> {
    let store = context
        .runtime
        .artifact_store()
        .ok_or_else(|| ViewImageToolError::missing_capability("artifact reads"))?;
    let mut chunks = store
        .open(
            context.operation,
            OpenArtifactRequest {
                artifact_id: artifact_id.clone(),
                offset: 0,
                max_bytes: ARTIFACT_CHUNK_BYTES,
            },
        )
        .await?;
    let mut bytes = Vec::new();
    let mut expected_offset = 0_u64;
    let mut reached_eof = false;
    loop {
        let chunk = tokio::select! {
            () = context.operation.cancelled() => {
                return Err(ViewImageToolError::cancelled("image read was cancelled"));
            }
            chunk = chunks.next() => chunk,
        };
        let Some(chunk) = chunk else {
            break;
        };
        let chunk = chunk?;
        if chunk.artifact_id != artifact_id {
            return Err(ViewImageToolError::process(
                "artifact stream returned a chunk for a different artifact",
            ));
        }
        if chunk.offset != expected_offset {
            return Err(ViewImageToolError::process(format!(
                "artifact stream was out of order: expected offset {expected_offset}, received {}",
                chunk.offset
            )));
        }
        let chunk_bytes = STANDARD.decode(chunk.data.0).map_err(|error| {
            ViewImageToolError::process(format!("artifact contained invalid base64 data: {error}"))
        })?;
        expected_offset = expected_offset.saturating_add(chunk_bytes.len() as u64);
        bytes.extend_from_slice(&chunk_bytes);
        if chunk.eof {
            reached_eof = true;
            break;
        }
    }
    if !reached_eof {
        return Err(ViewImageToolError::process(
            "artifact stream ended before the end-of-file marker",
        ));
    }
    if expected_offset != expected_size {
        return Err(ViewImageToolError::process(format!(
            "artifact size changed while reading: expected {expected_size} bytes, received {expected_offset}"
        )));
    }
    Ok(bytes)
}

fn supported_mime_type(format: ImageFormat) -> Result<&'static str, ViewImageToolError> {
    match format {
        ImageFormat::Jpeg => Ok("image/jpeg"),
        ImageFormat::Png => Ok("image/png"),
        ImageFormat::Gif => Ok("image/gif"),
        ImageFormat::WebP => Ok("image/webp"),
        _ => Err(ViewImageToolError::invalid_image(
            VIEW_IMAGE_INVALID_MESSAGE,
        )),
    }
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
                    "image_url": {
                        "type": "string",
                        "description": "Data URL for the loaded image."
                    },
                    "detail": {
                        "type": "string",
                        "enum": ["high", "original"],
                        "description": "Image detail hint returned by view_image. Returns `high` for default resized behavior or `original` when original resolution is preserved."
                    }
                },
                "required": ["image_url", "detail"],
                "additionalProperties": false
            })
            .as_object()
            .expect("output schema is an object")
            .clone(),
        ),
        strict: Some(false),
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
) -> Result<PathSpec, ViewImageToolError> {
    let root = descriptor
        .workspace_roots
        .iter()
        .find(|root| &root.id == workspace_root_id)
        .ok_or_else(|| {
            ViewImageToolError::invalid_path(format!(
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

fn normalize_relative_path(base: &str, input: &str) -> Result<String, ViewImageToolError> {
    if input.trim().is_empty() {
        return Err(ViewImageToolError::invalid_path("path must not be empty"));
    }
    if input.contains('\0') {
        return Err(ViewImageToolError::invalid_path(
            "path must not contain a null byte",
        ));
    }
    if input.starts_with('/') || is_windows_absolute(input) {
        return Err(ViewImageToolError::invalid_path(
            "absolute path could not be resolved inside the active workspace root",
        ));
    }

    let mut segments = Vec::new();
    for segment in base.split('/').chain(input.split('/')) {
        match segment {
            "" | "." => {}
            ".." => {
                if segments.pop().is_none() {
                    return Err(ViewImageToolError::invalid_path(
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
) -> Result<String, ViewImageToolError> {
    let root_url = Url::parse(root_uri).map_err(|_| {
        ViewImageToolError::invalid_path("workspace root has an invalid absolute URI")
    })?;
    if root_url.scheme() != "file" {
        return Err(ViewImageToolError::invalid_path(
            "absolute tool paths require a file-backed workspace root",
        ));
    }

    let mut root = if windows {
        root_url.path().to_owned()
    } else {
        root_url
            .to_file_path()
            .map_err(|()| {
                ViewImageToolError::invalid_path(
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
        return Err(ViewImageToolError::invalid_path(
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
