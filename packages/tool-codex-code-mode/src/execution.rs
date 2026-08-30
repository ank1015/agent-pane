use std::time::Instant;

use codex_code_mode_runtime::{
    CellId, CodeModeSession, DEFAULT_EXEC_YIELD_TIME_MS, DEFAULT_WAIT_YIELD_TIME_MS,
    ExecuteRequest, ToolDefinition as RuntimeToolDefinition, WaitRequest,
};
use llm_contracts::{ToolArguments, ToolDefinition};
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::{
    CodeModeToolError, CodeModeToolOutput, ParsedExecSource, collect_runtime_tool_definitions,
    parse_exec_source,
};

/// Harness-owned inputs shared by `exec` and `wait` execution.
pub struct CodeModeToolContext<'a> {
    session: &'a dyn CodeModeSession,
    nested_tools: Vec<RuntimeToolDefinition>,
    tool_call_id: String,
    default_exec_yield_time_ms: u64,
}

impl<'a> CodeModeToolContext<'a> {
    pub fn new(
        session: &'a dyn CodeModeSession,
        tool_call_id: impl Into<String>,
    ) -> Result<Self, CodeModeToolError> {
        let tool_call_id = tool_call_id.into();
        if tool_call_id.trim().is_empty() {
            return Err(CodeModeToolError::invalid_arguments(
                "tool_call_id must not be empty",
            ));
        }
        Ok(Self {
            session,
            nested_tools: Vec::new(),
            tool_call_id,
            default_exec_yield_time_ms: DEFAULT_EXEC_YIELD_TIME_MS,
        })
    }

    #[must_use]
    pub fn with_nested_tools(mut self, nested_tools: &[ToolDefinition]) -> Self {
        self.nested_tools = collect_runtime_tool_definitions(nested_tools);
        self
    }

    /// Installs namespace-aware definitions that are already in runtime form.
    #[must_use]
    pub fn with_runtime_nested_tools(mut self, nested_tools: &[RuntimeToolDefinition]) -> Self {
        self.nested_tools = nested_tools.to_vec();
        self
    }

    #[must_use]
    pub const fn with_default_exec_yield_time_ms(mut self, yield_time_ms: u64) -> Self {
        self.default_exec_yield_time_ms = yield_time_ms;
        self
    }

    #[must_use]
    pub const fn session(&self) -> &dyn CodeModeSession {
        self.session
    }

    #[must_use]
    pub fn nested_tools(&self) -> &[RuntimeToolDefinition] {
        &self.nested_tools
    }

    #[must_use]
    pub fn tool_call_id(&self) -> &str {
        &self.tool_call_id
    }
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct WaitArguments {
    pub cell_id: String,
    #[serde(default = "default_wait_yield_time_ms")]
    pub yield_time_ms: u64,
    #[serde(default)]
    pub max_tokens: Option<usize>,
    #[serde(default)]
    pub terminate: bool,
}

pub fn parse_exec_arguments(
    arguments: &ToolArguments,
) -> Result<ParsedExecSource, CodeModeToolError> {
    let ToolArguments::String(source) = arguments else {
        return Err(CodeModeToolError::invalid_arguments(
            "exec expects raw JavaScript source text, not a JSON object",
        ));
    };
    parse_exec_source(source).map_err(CodeModeToolError::invalid_arguments)
}

pub fn parse_wait_arguments(arguments: &ToolArguments) -> Result<WaitArguments, CodeModeToolError> {
    let ToolArguments::Object(arguments) = arguments else {
        return Err(CodeModeToolError::invalid_arguments(
            "wait expects JSON arguments",
        ));
    };
    serde_json::from_value(Value::Object(arguments.clone()))
        .map_err(CodeModeToolError::invalid_arguments)
}

pub async fn execute_exec_tool(
    arguments: &ToolArguments,
    context: &CodeModeToolContext<'_>,
) -> Result<CodeModeToolOutput, CodeModeToolError> {
    execute_exec(parse_exec_arguments(arguments)?, context).await
}

pub async fn execute_exec(
    parsed: ParsedExecSource,
    context: &CodeModeToolContext<'_>,
) -> Result<CodeModeToolOutput, CodeModeToolError> {
    let started_at = Instant::now();
    let max_output_tokens = parsed.max_output_tokens;
    let started = context
        .session
        .execute(ExecuteRequest {
            tool_call_id: context.tool_call_id.clone(),
            enabled_tools: context.nested_tools.clone(),
            source: parsed.code,
            yield_time_ms: Some(
                parsed
                    .yield_time_ms
                    .unwrap_or(context.default_exec_yield_time_ms),
            ),
            max_output_tokens,
        })
        .await
        .map_err(CodeModeToolError::runtime)?;
    let response = started
        .initial_response()
        .await
        .map_err(CodeModeToolError::runtime)?;
    Ok(CodeModeToolOutput::from_runtime(
        response,
        max_output_tokens,
        started_at.elapsed(),
    ))
}

pub async fn execute_wait_tool(
    arguments: &ToolArguments,
    context: &CodeModeToolContext<'_>,
) -> Result<CodeModeToolOutput, CodeModeToolError> {
    execute_wait(parse_wait_arguments(arguments)?, context).await
}

pub async fn execute_wait(
    arguments: WaitArguments,
    context: &CodeModeToolContext<'_>,
) -> Result<CodeModeToolOutput, CodeModeToolError> {
    let started_at = Instant::now();
    let cell_id = CellId::new(arguments.cell_id);
    let outcome = if arguments.terminate {
        context.session.terminate(cell_id).await
    } else {
        context
            .session
            .wait(WaitRequest {
                cell_id,
                yield_time_ms: arguments.yield_time_ms,
            })
            .await
    }
    .map_err(CodeModeToolError::runtime)?;
    Ok(CodeModeToolOutput::from_runtime(
        outcome.into(),
        arguments.max_tokens,
        started_at.elapsed(),
    ))
}

const fn default_wait_yield_time_ms() -> u64 {
    DEFAULT_WAIT_YIELD_TIME_MS
}
