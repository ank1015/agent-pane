use std::time::{SystemTime, UNIX_EPOCH};

use codex_code_mode_runtime::{
    CodeModeNestedToolCall, CodeModeToolKind, ToolDefinition as RuntimeToolDefinition,
};
use execution_runtime::{ExecutionRuntime, OperationContext};
use futures_util::future::try_join_all;
use llm_contracts::{
    AssistantContent, ContentPart, ImageDetail, ImageSource, MessageId, TextContent, Timestamp,
    ToolArguments, ToolDefinition, ToolResultError, ToolResultMessage, ToolResultOutcome,
};
use serde_json::{Value, json};
use uuid::Uuid;

use super::{CodexExecutionTarget, tool_admission::ToolAdmissionGate};

/// Machine and cancellation inputs shared by the stateless Codex tools.
pub struct StatelessToolContext<'a> {
    pub runtime: &'a dyn ExecutionRuntime,
    pub execution: &'a CodexExecutionTarget,
    pub operation: &'a OperationContext,
}

/// Executes tools that do not require durable harness-owned state.
#[derive(Default)]
pub struct StatelessToolExecutor;

impl StatelessToolExecutor {
    #[must_use]
    pub const fn new() -> Self {
        Self
    }

    /// Executes one direct assistant tool call and converts every tool failure
    /// into a model-visible error result. Passing non-tool assistant content is
    /// a harness programming error.
    pub async fn execute_tool_call(
        &self,
        call: &AssistantContent,
        context: &StatelessToolContext<'_>,
    ) -> Result<ToolResultMessage, StatelessToolDispatchError> {
        let admission = ToolAdmissionGate::default();
        self.execute_tool_call_with_admission(call, context, &admission)
            .await
    }

    async fn execute_tool_call_with_admission(
        &self,
        call: &AssistantContent,
        context: &StatelessToolContext<'_>,
        admission: &ToolAdmissionGate,
    ) -> Result<ToolResultMessage, StatelessToolDispatchError> {
        let AssistantContent::ToolCall { name, .. } = call else {
            return Err(StatelessToolDispatchError::NotAToolCall);
        };
        let _admission = admission.acquire(name).await;
        self.execute_tool_call_admitted(call, context).await
    }

    pub(super) async fn execute_tool_call_admitted(
        &self,
        call: &AssistantContent,
        context: &StatelessToolContext<'_>,
    ) -> Result<ToolResultMessage, StatelessToolDispatchError> {
        let AssistantContent::ToolCall {
            name,
            arguments,
            tool_call_id,
        } = call
        else {
            return Err(StatelessToolDispatchError::NotAToolCall);
        };

        let result = execute_without_gate(name, arguments, context).await;
        let (content, details, outcome) = result.into_transcript_parts();
        Ok(ToolResultMessage {
            id: MessageId::new(format!("tool-result-{}", Uuid::now_v7()))
                .expect("UUID-backed tool result ID is valid"),
            tool_name: name.clone(),
            tool_call_id: tool_call_id.clone(),
            content,
            details,
            timestamp: Timestamp(now_ms()),
            outcome,
        })
    }

    /// Executes all tool calls in an assistant response concurrently subject to
    /// each tool's Codex parallel-safety policy. `try_join_all` retains the
    /// assistant's original call order even when completion order differs.
    pub async fn execute_tool_calls(
        &self,
        content: &[AssistantContent],
        context: &StatelessToolContext<'_>,
    ) -> Result<Vec<ToolResultMessage>, StatelessToolDispatchError> {
        let admission = ToolAdmissionGate::default();
        try_join_all(
            content
                .iter()
                .filter(|item| matches!(item, AssistantContent::ToolCall { .. }))
                .map(|item| self.execute_tool_call_with_admission(item, context, &admission)),
        )
        .await
    }

    /// Executes a call originating inside a code-mode JavaScript cell and
    /// returns the value observed by that JavaScript invocation.
    pub async fn execute_nested_tool_call(
        &self,
        invocation: &CodeModeNestedToolCall,
        context: &StatelessToolContext<'_>,
    ) -> Result<Value, String> {
        let admission = ToolAdmissionGate::default();
        let _admission = if invocation.tool_name.namespace.is_some() {
            admission.acquire_exclusive().await
        } else {
            admission.acquire(&invocation.tool_name.name).await
        };
        self.execute_nested_tool_call_admitted(invocation, context)
            .await
    }

    pub(super) async fn execute_nested_tool_call_admitted(
        &self,
        invocation: &CodeModeNestedToolCall,
        context: &StatelessToolContext<'_>,
    ) -> Result<Value, String> {
        if invocation.tool_name.namespace.is_some() {
            return Err(format!(
                "unknown nested tool `{}`",
                display_nested_tool_name(invocation)
            ));
        }
        let arguments = nested_arguments(invocation)?;
        match execute_without_gate(&invocation.tool_name.name, &arguments, context).await {
            StatelessToolResult::Success(output) => {
                nested_success_value(&invocation.tool_name.name, &output)
            }
            StatelessToolResult::Error(error) => Err(error.message),
        }
    }
}

#[derive(Debug, thiserror::Error)]
pub enum StatelessToolDispatchError {
    #[error("assistant content is not a tool call")]
    NotAToolCall,
}

struct StatelessToolOutput {
    content: Vec<ContentPart>,
    details: Option<Value>,
}

struct StatelessToolError {
    name: &'static str,
    message: String,
    details: Option<Value>,
}

enum StatelessToolResult {
    Success(StatelessToolOutput),
    Error(StatelessToolError),
}

impl StatelessToolResult {
    fn into_transcript_parts(self) -> (Vec<ContentPart>, Option<Value>, ToolResultOutcome) {
        match self {
            Self::Success(output) => (output.content, output.details, ToolResultOutcome::Success),
            Self::Error(error) => (
                vec![text_content(error.message.clone())],
                error.details,
                ToolResultOutcome::Error {
                    error: ToolResultError {
                        message: error.message,
                        name: Some(error.name.to_owned()),
                    },
                },
            ),
        }
    }
}

async fn execute_without_gate(
    name: &str,
    arguments: &ToolArguments,
    context: &StatelessToolContext<'_>,
) -> StatelessToolResult {
    match name {
        tool_codex_apply_patch::TOOL_NAME => execute_apply_patch(arguments, context).await,
        tool_codex_view_image::TOOL_NAME => execute_view_image(arguments, context).await,
        _ => StatelessToolResult::Error(StatelessToolError {
            name: "unknown_tool",
            message: format!("Unknown tool `{name}`"),
            details: None,
        }),
    }
}

async fn execute_apply_patch(
    arguments: &ToolArguments,
    context: &StatelessToolContext<'_>,
) -> StatelessToolResult {
    let tool_context = match tool_codex_apply_patch::ApplyPatchToolContext::new(
        context.runtime,
        context.operation,
        context.execution.workspace_root_id.clone(),
        &context.execution.cwd,
    ) {
        Ok(tool_context) => tool_context,
        Err(error) => return apply_patch_error(error),
    };
    match tool_codex_apply_patch::execute_apply_patch_tool(arguments, &tool_context).await {
        Ok(output) => StatelessToolResult::Success(StatelessToolOutput {
            content: output.content,
            details: output.details,
        }),
        Err(error) => apply_patch_error(error),
    }
}

fn apply_patch_error(error: tool_codex_apply_patch::ApplyPatchToolError) -> StatelessToolResult {
    let (name, message, details) = error.into_parts();
    StatelessToolResult::Error(StatelessToolError {
        name,
        message,
        details,
    })
}

async fn execute_view_image(
    arguments: &ToolArguments,
    context: &StatelessToolContext<'_>,
) -> StatelessToolResult {
    let tool_context = match tool_codex_view_image::ViewImageToolContext::new(
        context.runtime,
        context.operation,
        context.execution.workspace_root_id.clone(),
        &context.execution.cwd,
    ) {
        Ok(tool_context) => tool_context,
        Err(error) => return view_image_error(error),
    };
    match tool_codex_view_image::execute_view_image_tool(arguments, &tool_context).await {
        Ok(output) => StatelessToolResult::Success(StatelessToolOutput {
            content: output.content,
            details: output.details,
        }),
        Err(error) => view_image_error(error),
    }
}

fn view_image_error(error: tool_codex_view_image::ViewImageToolError) -> StatelessToolResult {
    let (name, message, details) = error.into_parts();
    StatelessToolResult::Error(StatelessToolError {
        name,
        message,
        details,
    })
}

fn nested_arguments(invocation: &CodeModeNestedToolCall) -> Result<ToolArguments, String> {
    match invocation.tool_kind {
        CodeModeToolKind::Function => match &invocation.input {
            None => Ok(ToolArguments::Object(serde_json::Map::new())),
            Some(Value::Object(arguments)) => Ok(ToolArguments::Object(arguments.clone())),
            Some(_) => Err(format!(
                "tool `{}` expects a JSON object for arguments",
                display_nested_tool_name(invocation)
            )),
        },
        CodeModeToolKind::Freeform => match &invocation.input {
            Some(Value::String(input)) => Ok(ToolArguments::String(input.clone())),
            _ => Err(format!(
                "tool `{}` expects a string input",
                display_nested_tool_name(invocation)
            )),
        },
    }
}

fn nested_success_value(name: &str, output: &StatelessToolOutput) -> Result<Value, String> {
    match name {
        tool_codex_apply_patch::TOOL_NAME => Ok(json!({})),
        tool_codex_view_image::TOOL_NAME => {
            let Some(ContentPart::Image(image)) = output.content.first() else {
                return Err("view_image returned no image content".to_owned());
            };
            let image_url = match &image.source {
                ImageSource::Base64(source) => {
                    format!("data:{};base64,{}", source.mime_type, source.data)
                }
                ImageSource::Url(source) => source.url.clone(),
            };
            let mut value = json!({ "image_url": image_url });
            if let Some(detail) = image.detail {
                value["detail"] = Value::String(image_detail_name(detail).to_owned());
            }
            Ok(value)
        }
        _ => Err(format!("unknown nested tool `{name}`")),
    }
}

const fn image_detail_name(detail: ImageDetail) -> &'static str {
    match detail {
        ImageDetail::Auto => "auto",
        ImageDetail::Low => "low",
        ImageDetail::High => "high",
        ImageDetail::Original => "original",
    }
}

fn display_nested_tool_name(invocation: &CodeModeNestedToolCall) -> String {
    invocation.tool_name.namespace.as_ref().map_or_else(
        || invocation.tool_name.name.clone(),
        |namespace| format!("{namespace}{}", invocation.tool_name.name),
    )
}

fn text_content(content: impl Into<String>) -> ContentPart {
    ContentPart::Text(TextContent {
        content: content.into(),
        metadata: None,
    })
}

fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .ok()
        .and_then(|duration| u64::try_from(duration.as_millis()).ok())
        .unwrap_or(0)
}

/// Underlying tools installed into the code-mode runtime. They are embedded in
/// the `exec` description, not sent as top-level model tools.
#[must_use]
pub fn default_nested_tool_definitions() -> Vec<ToolDefinition> {
    let mut tools = vec![tool_codex_apply_patch::definition()];
    tools.extend(tool_codex_unified_exec::definitions());
    tools.push(tool_codex_view_image::definition());
    tools
}

/// Runtime definitions installed into code mode. Web search remains
/// namespaced as `web.run`, which V8 exposes as `tools.web__run`.
#[must_use]
pub fn code_mode_nested_tool_definitions(web_search_enabled: bool) -> Vec<RuntimeToolDefinition> {
    let mut tools =
        tool_codex_code_mode::collect_runtime_tool_definitions(&default_nested_tool_definitions());
    if web_search_enabled {
        tools.push(tool_codex_web_search::definition());
    }
    tools.sort_by(|left, right| left.name.cmp(&right.name));
    tools.dedup_by(|left, right| left.name == right.name);
    tools
}

/// Sol, Terra, and Luna are code-mode-only in the inspected Codex catalog.
#[must_use]
pub fn model_visible_tool_definitions(web_search_enabled: bool) -> Vec<ToolDefinition> {
    tool_codex_code_mode::definitions_from_runtime_tools(&code_mode_nested_tool_definitions(
        web_search_enabled,
    ))
}

#[cfg(test)]
mod tests {
    use codex_code_mode_runtime::{CellId, CodeModeNestedToolCall, CodeModeToolKind, ToolName};
    use execution_contracts::{MachineId, WorkspaceRootId};
    use execution_local::{LocalExecutionRuntime, LocalRuntimeConfig, LocalWorkspaceRoot};
    use execution_runtime::OperationContext;
    use image::{DynamicImage, ImageFormat, Rgb, RgbImage};
    use llm_contracts::{AssistantContent, ToolArguments, ToolCallId, ToolResultOutcome};
    use serde_json::{Value, json};
    use std::io::Cursor;

    use super::{
        StatelessToolContext, StatelessToolExecutor, code_mode_nested_tool_definitions,
        default_nested_tool_definitions, model_visible_tool_definitions,
    };
    use crate::runtime::CodexExecutionTarget;

    #[test]
    fn installs_four_nested_tools_and_exposes_only_exec_and_wait() {
        assert_eq!(
            default_nested_tool_definitions()
                .iter()
                .map(|tool| tool.name())
                .collect::<Vec<_>>(),
            ["apply_patch", "exec_command", "write_stdin", "view_image"]
        );

        let visible = model_visible_tool_definitions(false);
        assert_eq!(
            visible.iter().map(|tool| tool.name()).collect::<Vec<_>>(),
            ["exec", "wait"]
        );
        let description = match &visible[0] {
            llm_contracts::ToolDefinition::Custom(tool) => &tool.description,
            _ => panic!("exec is a custom tool"),
        };
        for name in ["apply_patch", "exec_command", "write_stdin", "view_image"] {
            assert!(description.contains(name), "missing {name} declaration");
        }
        assert!(!description.contains("tools.web__run"));
    }

    #[test]
    fn enabled_web_search_is_namespaced_in_runtime_and_exec_prompt() {
        let nested = code_mode_nested_tool_definitions(true);
        let web = nested
            .iter()
            .find(|tool| tool.name == tool_codex_web_search::CODE_MODE_TOOL_NAME)
            .expect("web.run definition");
        assert_eq!(web.tool_name.namespace.as_deref(), Some("web"));
        assert_eq!(web.tool_name.name, "run");
        assert_eq!(web.description, tool_codex_web_search::WEB_RUN_DESCRIPTION);

        let visible = model_visible_tool_definitions(true);
        let description = match &visible[0] {
            llm_contracts::ToolDefinition::Custom(tool) => &tool.description,
            _ => panic!("exec is a custom tool"),
        };
        assert!(description.contains("## web"));
        assert!(description.contains("### `web__run`"));
        assert!(description.contains("Tools in the web namespace."));
        assert!(description.contains(tool_codex_web_search::WEB_RUN_DESCRIPTION.trim()));
    }

    #[tokio::test]
    async fn applies_patch_and_preserves_batch_result_order() {
        let fixture = Fixture::new().await;
        let operation = OperationContext::new();
        let context = fixture.context(&operation);
        let calls = vec![
            call(
                "apply_patch",
                ToolArguments::String(
                    "*** Begin Patch\n*** Add File: created.txt\n+created\n*** End Patch"
                        .to_owned(),
                ),
                "patch-call",
            ),
            call(
                "missing_tool",
                ToolArguments::Object(serde_json::Map::new()),
                "missing-call",
            ),
            call(
                "view_image",
                object_arguments(json!({"path": "missing.png"})),
                "image-call",
            ),
        ];

        let results = StatelessToolExecutor::new()
            .execute_tool_calls(&calls, &context)
            .await
            .expect("tool calls");

        assert_eq!(
            results
                .iter()
                .map(|result| result.tool_call_id.as_str())
                .collect::<Vec<_>>(),
            ["patch-call", "missing-call", "image-call"]
        );
        assert_eq!(results[0].outcome, ToolResultOutcome::Success);
        assert_error(&results[1].outcome, "unknown_tool");
        assert_error(&results[2].outcome, "execution_error");
        assert_eq!(
            tokio::fs::read_to_string(fixture.workspace.join("project/created.txt"))
                .await
                .expect("created file"),
            "created\n"
        );
    }

    #[tokio::test]
    async fn nested_apply_patch_matches_codex_empty_object_result() {
        let fixture = Fixture::new().await;
        let operation = OperationContext::new();
        let invocation = CodeModeNestedToolCall {
            cell_id: CellId::new("cell-1"),
            runtime_tool_call_id: "runtime-call-1".to_owned(),
            tool_name: ToolName::plain("apply_patch"),
            tool_kind: CodeModeToolKind::Freeform,
            input: Some(Value::String(
                "*** Begin Patch\n*** Add File: nested.txt\n+nested\n*** End Patch".to_owned(),
            )),
        };

        let result = StatelessToolExecutor::new()
            .execute_nested_tool_call(&invocation, &fixture.context(&operation))
            .await
            .expect("nested patch");

        assert_eq!(result, json!({}));
        assert_eq!(
            tokio::fs::read_to_string(fixture.workspace.join("project/nested.txt"))
                .await
                .expect("nested file"),
            "nested\n"
        );
    }

    #[tokio::test]
    async fn nested_view_image_returns_codex_data_url_and_detail() {
        let fixture = Fixture::new().await;
        let mut encoded = Cursor::new(Vec::new());
        DynamicImage::ImageRgb8(RgbImage::from_pixel(1, 1, Rgb([12, 34, 56])))
            .write_to(&mut encoded, ImageFormat::Png)
            .expect("encode PNG");
        tokio::fs::write(
            fixture.workspace.join("project/image.png"),
            encoded.into_inner(),
        )
        .await
        .expect("write PNG");
        let operation = OperationContext::new();
        let invocation = CodeModeNestedToolCall {
            cell_id: CellId::new("cell-1"),
            runtime_tool_call_id: "runtime-call-1".to_owned(),
            tool_name: ToolName::plain("view_image"),
            tool_kind: CodeModeToolKind::Function,
            input: Some(json!({"path": "image.png", "detail": "original"})),
        };

        let result = StatelessToolExecutor::new()
            .execute_nested_tool_call(&invocation, &fixture.context(&operation))
            .await
            .expect("nested image");

        assert_eq!(result["detail"], "original");
        assert!(
            result["image_url"]
                .as_str()
                .expect("image URL")
                .starts_with("data:application/octet-stream;base64,")
        );
    }

    #[tokio::test]
    async fn nested_calls_enforce_codex_input_kinds_and_surface_tool_errors() {
        let fixture = Fixture::new().await;
        let operation = OperationContext::new();
        let executor = StatelessToolExecutor::new();
        let wrong_kind = CodeModeNestedToolCall {
            cell_id: CellId::new("cell-1"),
            runtime_tool_call_id: "runtime-call-1".to_owned(),
            tool_name: ToolName::plain("apply_patch"),
            tool_kind: CodeModeToolKind::Freeform,
            input: Some(json!({"patch": "not raw"})),
        };
        assert_eq!(
            executor
                .execute_nested_tool_call(&wrong_kind, &fixture.context(&operation))
                .await
                .expect_err("freeform type mismatch"),
            "tool `apply_patch` expects a string input"
        );

        let invalid_patch = CodeModeNestedToolCall {
            input: Some(Value::String("not a patch".to_owned())),
            ..wrong_kind
        };
        assert!(
            executor
                .execute_nested_tool_call(&invalid_patch, &fixture.context(&operation))
                .await
                .expect_err("invalid patch")
                .contains("apply_patch verification failed")
        );
    }

    struct Fixture {
        _directory: tempfile::TempDir,
        workspace: std::path::PathBuf,
        runtime: LocalExecutionRuntime,
        execution: CodexExecutionTarget,
    }

    impl Fixture {
        async fn new() -> Self {
            let directory = tempfile::tempdir().expect("temporary directory");
            let workspace = directory.path().join("workspace");
            tokio::fs::create_dir_all(workspace.join("project"))
                .await
                .expect("workspace");
            let workspace = tokio::fs::canonicalize(workspace)
                .await
                .expect("canonical workspace");
            let root_id = WorkspaceRootId::new("root").expect("root ID");
            let runtime = LocalExecutionRuntime::new(LocalRuntimeConfig {
                machine_id: MachineId::new("machine").expect("machine ID"),
                name: "Codex harness stateless tool tests".to_owned(),
                state_directory: directory.path().join("state"),
                workspace_roots: vec![LocalWorkspaceRoot {
                    id: root_id.clone(),
                    name: "workspace".to_owned(),
                    path: workspace.clone(),
                    read_only: false,
                }],
                native_grants: Vec::new(),
            })
            .await
            .expect("local runtime");
            Self {
                _directory: directory,
                workspace,
                runtime,
                execution: CodexExecutionTarget {
                    machine_id: MachineId::new("machine").expect("machine ID"),
                    workspace_root_id: root_id,
                    cwd: "project".to_owned(),
                },
            }
        }

        fn context<'a>(&'a self, operation: &'a OperationContext) -> StatelessToolContext<'a> {
            StatelessToolContext {
                runtime: &self.runtime,
                execution: &self.execution,
                operation,
            }
        }
    }

    fn call(name: &str, arguments: ToolArguments, tool_call_id: &str) -> AssistantContent {
        AssistantContent::ToolCall {
            name: name.to_owned(),
            arguments,
            tool_call_id: ToolCallId::new(tool_call_id).expect("tool call ID"),
        }
    }

    fn object_arguments(value: Value) -> ToolArguments {
        ToolArguments::Object(value.as_object().expect("object arguments").clone())
    }

    fn assert_error(outcome: &ToolResultOutcome, expected_name: &str) {
        let ToolResultOutcome::Error { error } = outcome else {
            panic!("expected tool error")
        };
        assert_eq!(error.name.as_deref(), Some(expected_name));
    }
}
