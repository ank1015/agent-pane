pub mod bash;
pub mod edit;
pub mod read;
mod truncate;
pub mod write;

use std::time::{SystemTime, UNIX_EPOCH};

use execution_contracts::{
    MachineDescriptor, OperationId, PathConvention, PathSpec, WorkspaceRootId,
};
use execution_runtime::{ExecutionRuntime, OperationContext};
use llm_contracts::{
    AssistantContent, ContentPart, FunctionTool, MessageId, TextContent, Timestamp, ToolArguments,
    ToolDefinition, ToolResultError, ToolResultMessage, ToolResultOutcome,
};
use serde::de::DeserializeOwned;
use serde_json::Value;
use url::Url;
use uuid::Uuid;

pub struct ToolExecutionContext<'a> {
    pub runtime: &'a dyn ExecutionRuntime,
    pub cwd: &'a WorkspaceCwd,
    pub operation: &'a OperationContext,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct WorkspaceCwd {
    root_id: WorkspaceRootId,
    path: String,
}

impl WorkspaceCwd {
    pub fn new(
        root_id: WorkspaceRootId,
        path: impl Into<String>,
    ) -> Result<Self, ToolExecutionError> {
        let path = normalize_relative_path(".", &path.into())?;
        Ok(Self { root_id, path })
    }

    #[must_use]
    pub fn root_id(&self) -> &WorkspaceRootId {
        &self.root_id
    }

    #[must_use]
    pub fn path(&self) -> &str {
        &self.path
    }

    fn resolve(
        &self,
        descriptor: &MachineDescriptor,
        input: &str,
    ) -> Result<PathSpec, ToolExecutionError> {
        let root = descriptor
            .workspace_roots
            .iter()
            .find(|root| root.id == self.root_id)
            .ok_or_else(|| {
                ToolExecutionError::invalid_path(format!(
                    "workspace root `{}` is not exposed by the execution runtime",
                    self.root_id
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
        let base = if was_absolute { "." } else { &self.path };
        Ok(PathSpec::workspace(
            self.root_id.clone(),
            normalize_relative_path(base, &input)?,
        ))
    }
}

#[derive(Debug, thiserror::Error)]
#[error("{message}")]
pub struct ToolExecutionError {
    name: &'static str,
    message: String,
    details: Option<Value>,
}

impl ToolExecutionError {
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

    pub(crate) fn invalid_arguments(tool: &str, message: impl std::fmt::Display) -> Self {
        Self::new(
            "invalid_arguments",
            format!("Invalid arguments for {tool}: {message}"),
        )
    }

    pub(crate) fn invalid_path(message: impl Into<String>) -> Self {
        Self::new("invalid_path", message)
    }

    pub(crate) fn missing_capability(capability: &str) -> Self {
        Self::new(
            "unsupported_capability",
            format!("Execution runtime does not support {capability}"),
        )
    }

    pub(crate) fn command(message: impl Into<String>, details: Option<Value>) -> Self {
        let error = Self::new("command_failed", message);
        match details {
            Some(details) => error.with_details(details),
            None => error,
        }
    }

    pub(crate) fn cancelled(message: impl Into<String>) -> Self {
        Self::new("cancelled", message)
    }

    pub(crate) fn process(message: impl Into<String>) -> Self {
        Self::new("process_error", message)
    }

    pub(crate) fn tool(name: &'static str, message: impl Into<String>) -> Self {
        Self::new(name, message)
    }

    fn into_parts(self) -> (&'static str, String, Option<Value>) {
        (self.name, self.message, self.details)
    }
}

impl From<execution_contracts::ExecutionError> for ToolExecutionError {
    fn from(error: execution_contracts::ExecutionError) -> Self {
        let details = serde_json::to_value(&error).ok();
        Self {
            name: "execution_error",
            message: error.message,
            details,
        }
    }
}

pub struct ToolOutput {
    pub content: Vec<ContentPart>,
    pub details: Option<Value>,
}

impl ToolOutput {
    pub(crate) fn text(content: impl Into<String>) -> Self {
        Self {
            content: vec![text_content(content)],
            details: None,
        }
    }

    pub(crate) fn with_details(mut self, details: Value) -> Self {
        self.details = Some(details);
        self
    }
}

pub fn default_tool_definitions() -> Vec<ToolDefinition> {
    vec![
        read::definition(),
        bash::definition(),
        edit::definition(),
        write::definition(),
    ]
}

pub async fn execute_tool_call(
    call: &AssistantContent,
    context: &ToolExecutionContext<'_>,
) -> Result<ToolResultMessage, ToolExecutionError> {
    let AssistantContent::ToolCall {
        name,
        arguments,
        tool_call_id,
    } = call
    else {
        return Err(ToolExecutionError::new(
            "invalid_tool_call",
            "assistant content is not a tool call",
        ));
    };

    let result = match name.as_str() {
        "bash" => bash::execute_bash_tool(arguments, context).await,
        "read" => read::execute_read_tool(arguments, context).await,
        "write" => write::execute_write_tool(arguments, context).await,
        "edit" => edit::execute_edit_tool(arguments, context).await,
        _ => Err(ToolExecutionError::new(
            "unknown_tool",
            format!("Unknown tool `{name}`"),
        )),
    };

    let (content, details, outcome) = match result {
        Ok(output) => (output.content, output.details, ToolResultOutcome::Success),
        Err(error) => {
            let (error_name, message, details) = error.into_parts();
            (
                vec![text_content(message.clone())],
                details,
                ToolResultOutcome::Error {
                    error: ToolResultError {
                        message,
                        name: Some(error_name.to_owned()),
                    },
                },
            )
        }
    };

    Ok(ToolResultMessage {
        id: MessageId::new(format!("tool-result-{}", Uuid::now_v7()))
            .expect("UUID tool result identifier is valid"),
        tool_name: name.clone(),
        tool_call_id: tool_call_id.clone(),
        content,
        details,
        timestamp: Timestamp(now_ms()),
        outcome,
    })
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

pub(crate) fn parse_arguments<T: DeserializeOwned>(
    tool: &str,
    arguments: &ToolArguments,
) -> Result<T, ToolExecutionError> {
    match arguments {
        ToolArguments::Object(arguments) => {
            serde_json::from_value(Value::Object(arguments.clone()))
        }
        ToolArguments::String(arguments) => serde_json::from_str(arguments),
    }
    .map_err(|error| ToolExecutionError::invalid_arguments(tool, error))
}

pub(crate) fn resolve_path(
    context: &ToolExecutionContext<'_>,
    input: &str,
) -> Result<PathSpec, ToolExecutionError> {
    context.cwd.resolve(context.runtime.descriptor(), input)
}

pub(crate) fn operation_id(prefix: &str) -> OperationId {
    OperationId::new(format!("{prefix}-{}", Uuid::now_v7()))
        .expect("UUID operation identifier is valid")
}

pub(crate) fn text_content(content: impl Into<String>) -> ContentPart {
    ContentPart::Text(TextContent {
        content: content.into(),
        metadata: None,
    })
}

fn normalize_relative_path(base: &str, input: &str) -> Result<String, ToolExecutionError> {
    if input.trim().is_empty() {
        return Err(ToolExecutionError::invalid_path("path must not be empty"));
    }
    if input.contains('\0') {
        return Err(ToolExecutionError::invalid_path(
            "path must not contain a null byte",
        ));
    }
    if input.starts_with('/') || is_windows_absolute(input) {
        return Err(ToolExecutionError::invalid_path(
            "absolute path could not be resolved inside the active workspace root",
        ));
    }

    let mut segments = Vec::new();
    for segment in base.split('/').chain(input.split('/')) {
        match segment {
            "" | "." => {}
            ".." => {
                if segments.pop().is_none() {
                    return Err(ToolExecutionError::invalid_path(
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
) -> Result<String, ToolExecutionError> {
    let root_url = Url::parse(root_uri).map_err(|_| {
        ToolExecutionError::invalid_path("workspace root has an invalid absolute URI")
    })?;
    if root_url.scheme() != "file" {
        return Err(ToolExecutionError::invalid_path(
            "absolute tool paths require a file-backed workspace root",
        ));
    }

    let mut root = if windows {
        root_url.path().to_owned()
    } else {
        root_url
            .to_file_path()
            .map_err(|()| {
                ToolExecutionError::invalid_path(
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
        return Err(ToolExecutionError::invalid_path(
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

fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .ok()
        .and_then(|duration| u64::try_from(duration.as_millis()).ok())
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use llm_contracts::Validate;

    use super::default_tool_definitions;

    #[test]
    fn default_definitions_are_valid_and_ordered() {
        let tools = default_tool_definitions();
        let names: Vec<_> = tools.iter().map(|tool| tool.name()).collect();

        assert_eq!(names, ["read", "bash", "edit", "write"]);
        for tool in tools {
            tool.validate().expect("default tool definition is valid");
        }
    }
}
