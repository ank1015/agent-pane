use std::time::{SystemTime, UNIX_EPOCH};

use execution_contracts::WorkspaceRootId;
use execution_runtime::{ExecutionRuntime, OperationContext};
use llm_contracts::{
    AssistantContent, ContentPart, MessageId, TextContent, Timestamp, ToolArguments,
    ToolDefinition, ToolResultError, ToolResultMessage, ToolResultOutcome,
};
use serde_json::Value;
use uuid::Uuid;

pub struct ToolExecutionContext<'a> {
    pub runtime: &'a dyn ExecutionRuntime,
    pub cwd: &'a WorkspaceCwd,
    pub operation: &'a OperationContext,
    pub search: &'a tool_firecrawl_search::FirecrawlSearchToolContext,
    pub scrape: &'a tool_firecrawl_scrape::FirecrawlScrapeToolContext,
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

    fn invalid_path(message: impl Into<String>) -> Self {
        Self::new("invalid_path", message)
    }

    fn into_parts(self) -> (&'static str, String, Option<Value>) {
        (self.name, self.message, self.details)
    }
}

impl From<tool_pi_bash::BashToolError> for ToolExecutionError {
    fn from(error: tool_pi_bash::BashToolError) -> Self {
        let (name, message, details) = error.into_parts();
        Self {
            name,
            message,
            details,
        }
    }
}

impl From<tool_pi_edit::EditToolError> for ToolExecutionError {
    fn from(error: tool_pi_edit::EditToolError) -> Self {
        let (name, message, details) = error.into_parts();
        Self {
            name,
            message,
            details,
        }
    }
}

impl From<tool_pi_read::ReadToolError> for ToolExecutionError {
    fn from(error: tool_pi_read::ReadToolError) -> Self {
        let (name, message, details) = error.into_parts();
        Self {
            name,
            message,
            details,
        }
    }
}

impl From<tool_pi_write::WriteToolError> for ToolExecutionError {
    fn from(error: tool_pi_write::WriteToolError) -> Self {
        let (name, message, details) = error.into_parts();
        Self {
            name,
            message,
            details,
        }
    }
}

impl From<tool_firecrawl_search::FirecrawlSearchToolError> for ToolExecutionError {
    fn from(error: tool_firecrawl_search::FirecrawlSearchToolError) -> Self {
        let (name, message, details) = error.into_parts();
        Self {
            name,
            message,
            details,
        }
    }
}

impl From<tool_firecrawl_scrape::FirecrawlScrapeToolError> for ToolExecutionError {
    fn from(error: tool_firecrawl_scrape::FirecrawlScrapeToolError) -> Self {
        let (name, message, details) = error.into_parts();
        Self {
            name,
            message,
            details,
        }
    }
}

pub struct ToolOutput {
    pub content: Vec<ContentPart>,
    pub details: Option<Value>,
}

impl From<tool_pi_bash::BashToolOutput> for ToolOutput {
    fn from(output: tool_pi_bash::BashToolOutput) -> Self {
        Self {
            content: output.content,
            details: output.details,
        }
    }
}

impl From<tool_pi_edit::EditToolOutput> for ToolOutput {
    fn from(output: tool_pi_edit::EditToolOutput) -> Self {
        Self {
            content: output.content,
            details: output.details,
        }
    }
}

impl From<tool_pi_read::ReadToolOutput> for ToolOutput {
    fn from(output: tool_pi_read::ReadToolOutput) -> Self {
        Self {
            content: output.content,
            details: output.details,
        }
    }
}

impl From<tool_pi_write::WriteToolOutput> for ToolOutput {
    fn from(output: tool_pi_write::WriteToolOutput) -> Self {
        Self {
            content: output.content,
            details: output.details,
        }
    }
}

impl From<tool_firecrawl_search::FirecrawlSearchToolOutput> for ToolOutput {
    fn from(output: tool_firecrawl_search::FirecrawlSearchToolOutput) -> Self {
        Self {
            content: output.content,
            details: output.details,
        }
    }
}

impl From<tool_firecrawl_scrape::FirecrawlScrapeToolOutput> for ToolOutput {
    fn from(output: tool_firecrawl_scrape::FirecrawlScrapeToolOutput) -> Self {
        Self {
            content: output.content,
            details: output.details,
        }
    }
}

pub fn default_tool_definitions(web_search_enabled: bool) -> Vec<ToolDefinition> {
    let mut tools = vec![
        tool_pi_read::definition(),
        tool_pi_bash::definition(),
        tool_pi_edit::definition(),
        tool_pi_write::definition(),
    ];
    if web_search_enabled {
        tools.extend([
            tool_firecrawl_search::definition(),
            tool_firecrawl_scrape::definition(),
        ]);
    }
    tools
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
        "bash" => execute_bash_tool(arguments, context).await,
        "read" => execute_read_tool(arguments, context).await,
        "write" => execute_write_tool(arguments, context).await,
        "edit" => execute_edit_tool(arguments, context).await,
        tool_firecrawl_search::TOOL_NAME => {
            tool_firecrawl_search::execute_search_tool(arguments, context.search)
                .await
                .map(Into::into)
                .map_err(Into::into)
        }
        tool_firecrawl_scrape::TOOL_NAME => {
            tool_firecrawl_scrape::execute_scrape_tool(arguments, context.scrape)
                .await
                .map(Into::into)
                .map_err(Into::into)
        }
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

async fn execute_bash_tool(
    arguments: &ToolArguments,
    context: &ToolExecutionContext<'_>,
) -> Result<ToolOutput, ToolExecutionError> {
    let bash_context = tool_pi_bash::BashToolContext::new(
        context.runtime,
        context.operation,
        context.cwd.root_id().clone(),
        context.cwd.path(),
    )?;
    tool_pi_bash::execute_bash_tool(arguments, &bash_context)
        .await
        .map(Into::into)
        .map_err(Into::into)
}

async fn execute_edit_tool(
    arguments: &ToolArguments,
    context: &ToolExecutionContext<'_>,
) -> Result<ToolOutput, ToolExecutionError> {
    let edit_context = tool_pi_edit::EditToolContext::new(
        context.runtime,
        context.operation,
        context.cwd.root_id().clone(),
        context.cwd.path(),
    )?;
    tool_pi_edit::execute_edit_tool(arguments, &edit_context)
        .await
        .map(Into::into)
        .map_err(Into::into)
}

async fn execute_read_tool(
    arguments: &ToolArguments,
    context: &ToolExecutionContext<'_>,
) -> Result<ToolOutput, ToolExecutionError> {
    let read_context = tool_pi_read::ReadToolContext::new(
        context.runtime,
        context.operation,
        context.cwd.root_id().clone(),
        context.cwd.path(),
    )?;
    tool_pi_read::execute_read_tool(arguments, &read_context)
        .await
        .map(Into::into)
        .map_err(Into::into)
}

async fn execute_write_tool(
    arguments: &ToolArguments,
    context: &ToolExecutionContext<'_>,
) -> Result<ToolOutput, ToolExecutionError> {
    let write_context = tool_pi_write::WriteToolContext::new(
        context.runtime,
        context.operation,
        context.cwd.root_id().clone(),
        context.cwd.path(),
    )?;
    tool_pi_write::execute_write_tool(arguments, &write_context)
        .await
        .map(Into::into)
        .map_err(Into::into)
}

fn text_content(content: impl Into<String>) -> ContentPart {
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
        let tools = default_tool_definitions(true);
        let names: Vec<_> = tools.iter().map(|tool| tool.name()).collect();

        assert_eq!(names, ["read", "bash", "edit", "write", "search", "scrape"]);
        for tool in tools {
            tool.validate().expect("default tool definition is valid");
        }
    }

    #[test]
    fn omits_web_tools_when_disabled() {
        let tools = default_tool_definitions(false);
        let names = tools.iter().map(|tool| tool.name()).collect::<Vec<_>>();

        assert_eq!(names, ["read", "bash", "edit", "write"]);
    }
}
