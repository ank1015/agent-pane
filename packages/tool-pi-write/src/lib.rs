//! Reusable implementation of Pi's `write` tool.
//!
//! The crate owns the model-facing definition and the workspace mutation that
//! creates or overwrites a file. Harnesses remain responsible for selecting
//! the tool and recording its result.

use execution_contracts::{
    ApplyMutationRequest, ContentSource, MachineDescriptor, MutationAtomicity, MutationOperation,
    MutationPlan, MutationPostActions, OperationId, PathConvention, PathSpec, WorkspaceRootId,
};
use execution_runtime::{ExecutionRuntime, OperationContext};
use llm_contracts::{ContentPart, FunctionTool, TextContent, ToolArguments, ToolDefinition};
use serde::{Deserialize, Serialize, de::DeserializeOwned};
use serde_json::{Value, json};
use url::Url;
use uuid::Uuid;

/// Name used in the model-facing tool definition.
pub const TOOL_NAME: &str = "write";

/// Typed arguments accepted by the write tool.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct WriteArguments {
    pub path: String,
    pub content: String,
}

/// Execution inputs specific to the write tool.
pub struct WriteToolContext<'a> {
    runtime: &'a dyn ExecutionRuntime,
    operation: &'a OperationContext,
    workspace_root_id: WorkspaceRootId,
    cwd: String,
}

impl<'a> WriteToolContext<'a> {
    /// Creates a write context rooted at `workspace_root_id` and `cwd`.
    pub fn new(
        runtime: &'a dyn ExecutionRuntime,
        operation: &'a OperationContext,
        workspace_root_id: WorkspaceRootId,
        cwd: impl Into<String>,
    ) -> Result<Self, WriteToolError> {
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

    fn resolve_path(&self, input: &str) -> Result<PathSpec, WriteToolError> {
        resolve_path(
            self.runtime.descriptor(),
            &self.workspace_root_id,
            &self.cwd,
            input,
        )
    }
}

/// Successful output from the write tool.
#[derive(Clone, Debug, PartialEq)]
pub struct WriteToolOutput {
    pub content: Vec<ContentPart>,
    pub details: Option<Value>,
}

impl WriteToolOutput {
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

/// Structured write failure that a harness can map into its own tool result.
#[derive(Debug, thiserror::Error)]
#[error("{message}")]
pub struct WriteToolError {
    name: &'static str,
    message: String,
    details: Option<Value>,
}

impl WriteToolError {
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

    fn missing_capability(capability: &str) -> Self {
        Self::new(
            "unsupported_capability",
            format!("Execution runtime does not support {capability}"),
        )
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

impl From<execution_contracts::ExecutionError> for WriteToolError {
    fn from(error: execution_contracts::ExecutionError) -> Self {
        let details = serde_json::to_value(&error).ok();
        Self {
            name: "execution_error",
            message: error.message,
            details,
        }
    }
}

/// Returns the model-facing Pi write tool definition.
#[must_use]
pub fn definition() -> ToolDefinition {
    function_tool(
        TOOL_NAME,
        "Write content to a file in the current workspace. Creates the file if it doesn't exist, overwrites if it does, and automatically creates parent directories.",
        json!({
            "type": "object",
            "properties": {
                "path": {
                    "type": "string",
                    "description": "Path to the file to write (relative or absolute within the current workspace)"
                },
                "content": {
                    "type": "string",
                    "description": "Content to write to the file"
                }
            },
            "required": ["path", "content"]
        }),
    )
}

/// Parses provider-neutral LLM tool arguments into typed write arguments.
pub fn parse_arguments(arguments: &ToolArguments) -> Result<WriteArguments, WriteToolError> {
    parse(arguments).map_err(WriteToolError::invalid_arguments)
}

/// Parses and executes an LLM tool call's arguments.
pub async fn execute_write_tool(
    arguments: &ToolArguments,
    context: &WriteToolContext<'_>,
) -> Result<WriteToolOutput, WriteToolError> {
    execute(parse_arguments(arguments)?, context).await
}

/// Creates or overwrites a workspace file with typed arguments.
pub async fn execute(
    arguments: WriteArguments,
    context: &WriteToolContext<'_>,
) -> Result<WriteToolOutput, WriteToolError> {
    let path = context.resolve_path(&arguments.path)?;
    let mutation = context
        .runtime
        .workspace_mutation()
        .ok_or_else(|| WriteToolError::missing_capability("workspace mutations"))?;
    let bytes = arguments.content.len();
    let result = mutation
        .apply(
            context.operation,
            ApplyMutationRequest {
                operation_id: operation_id(),
                plan: MutationPlan {
                    operations: vec![MutationOperation::PutFile {
                        path,
                        content: ContentSource::Text {
                            content: arguments.content,
                        },
                        create_parents: true,
                        preserve_utf8_bom: false,
                        expected_revision: None,
                    }],
                    atomicity: MutationAtomicity::AtomicIfSupported,
                    post_actions: MutationPostActions::default(),
                },
            },
        )
        .await?;

    Ok(WriteToolOutput::text(format!(
        "Successfully wrote {bytes} bytes to {}",
        arguments.path
    ))
    .with_details(json!({ "mutation": result })))
}

fn operation_id() -> OperationId {
    OperationId::new(format!("pi-write-{}", Uuid::now_v7()))
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
) -> Result<PathSpec, WriteToolError> {
    let root = descriptor
        .workspace_roots
        .iter()
        .find(|root| &root.id == workspace_root_id)
        .ok_or_else(|| {
            WriteToolError::invalid_path(format!(
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

fn normalize_relative_path(base: &str, input: &str) -> Result<String, WriteToolError> {
    if input.trim().is_empty() {
        return Err(WriteToolError::invalid_path("path must not be empty"));
    }
    if input.contains('\0') {
        return Err(WriteToolError::invalid_path(
            "path must not contain a null byte",
        ));
    }
    if input.starts_with('/') || is_windows_absolute(input) {
        return Err(WriteToolError::invalid_path(
            "absolute path could not be resolved inside the active workspace root",
        ));
    }

    let mut segments = Vec::new();
    for segment in base.split('/').chain(input.split('/')) {
        match segment {
            "" | "." => {}
            ".." => {
                if segments.pop().is_none() {
                    return Err(WriteToolError::invalid_path(
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
) -> Result<String, WriteToolError> {
    let root_url = Url::parse(root_uri)
        .map_err(|_| WriteToolError::invalid_path("workspace root has an invalid absolute URI"))?;
    if root_url.scheme() != "file" {
        return Err(WriteToolError::invalid_path(
            "absolute tool paths require a file-backed workspace root",
        ));
    }

    let mut root = if windows {
        root_url.path().to_owned()
    } else {
        root_url
            .to_file_path()
            .map_err(|()| {
                WriteToolError::invalid_path(
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
        return Err(WriteToolError::invalid_path(
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
