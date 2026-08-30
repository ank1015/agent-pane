use std::sync::{Arc, Mutex};

use codex_code_mode_runtime::{
    CellId, CodeModeNestedToolCall, CodeModeSessionDelegate, InProcessCodeModeSession,
    NotificationFuture, ToolInvocationFuture,
};
use llm_contracts::{
    ContentPart, CustomTool, CustomToolFormat, FunctionTool, GrammarSyntax, ToolArguments,
    ToolDefinition,
};
use serde_json::{Map, Value, json};
use tokio_util::sync::CancellationToken;
use tool_codex_code_mode::{
    CodeModeToolContext, EXEC_LARK_GRAMMAR, EXEC_TOOL_NAME, WAIT_TOOL_NAME,
    collect_runtime_tool_definitions, definitions, execute_exec_tool, execute_wait_tool,
    parse_exec_source,
};

#[derive(Default)]
struct EchoDelegate {
    calls: Mutex<Vec<CodeModeNestedToolCall>>,
}

impl CodeModeSessionDelegate for EchoDelegate {
    fn invoke_tool<'a>(
        &'a self,
        invocation: CodeModeNestedToolCall,
        _cancellation_token: CancellationToken,
    ) -> ToolInvocationFuture<'a> {
        self.calls
            .lock()
            .expect("calls lock")
            .push(invocation.clone());
        Box::pin(async move {
            let value = invocation
                .input
                .and_then(|input| input.get("value").cloned())
                .unwrap_or(Value::Null);
            Ok(json!({ "answer": value }))
        })
    }

    fn notify<'a>(
        &'a self,
        _call_id: String,
        _cell_id: CellId,
        _text: String,
        _cancellation_token: CancellationToken,
    ) -> NotificationFuture<'a> {
        Box::pin(async { Ok(()) })
    }

    fn cell_closed(&self, _cell_id: &CellId) {}
}

fn echo_tool(name: &str) -> ToolDefinition {
    ToolDefinition::Function(FunctionTool {
        name: name.to_string(),
        description: "Returns an answer derived from the input.".to_string(),
        parameters: object(json!({
            "type": "object",
            "properties": {
                "value": { "type": "number" }
            },
            "required": ["value"],
            "additionalProperties": false
        })),
        output_schema: Some(object(json!({
            "type": "object",
            "properties": {
                "answer": { "type": "number" }
            },
            "required": ["answer"],
            "additionalProperties": false
        }))),
        strict: Some(false),
    })
}

fn object(value: Value) -> Map<String, Value> {
    value.as_object().expect("JSON object").clone()
}

fn text_parts(content: &[ContentPart]) -> Vec<&str> {
    content
        .iter()
        .filter_map(|part| match part {
            ContentPart::Text(text) => Some(text.content.as_str()),
            ContentPart::Image(_) => None,
        })
        .collect()
}

#[test]
fn definitions_expose_raw_exec_wait_and_typed_nested_tools() {
    let nested = vec![
        echo_tool("strange-tool"),
        ToolDefinition::Custom(CustomTool {
            name: "apply_patch".to_string(),
            description: "Apply a patch.".to_string(),
            format: CustomToolFormat {
                syntax: GrammarSyntax::Lark,
                definition: "start: /[\\s\\S]+/".to_string(),
            },
        }),
    ];
    let definitions = definitions(&nested);

    let ToolDefinition::Custom(exec) = &definitions[0] else {
        panic!("exec should be custom")
    };
    assert_eq!(exec.name, EXEC_TOOL_NAME);
    assert_eq!(exec.format.definition, EXEC_LARK_GRAMMAR);
    assert!(exec.description.contains("strange_tool(args:"));
    assert!(exec.description.contains("Promise<{ answer: number; }>"));
    assert!(exec.description.contains("apply_patch(input: string)"));

    let ToolDefinition::Function(wait) = &definitions[1] else {
        panic!("wait should be a function")
    };
    assert_eq!(wait.name, WAIT_TOOL_NAME);
    assert_eq!(wait.parameters["required"], json!(["cell_id"]));
    assert!(wait.parameters["properties"].get("max_tokens").is_some());

    let runtime = collect_runtime_tool_definitions(&nested);
    assert_eq!(runtime[1].name, "strange_tool");
    assert_eq!(runtime[1].tool_name.name, "strange-tool");
    assert!(runtime[1].description.contains("exec tool declaration:"));
}

#[test]
fn exec_pragma_is_strict_and_preserves_the_remaining_source() {
    let parsed = parse_exec_source(
        "// @exec: {\"yield_time_ms\": 25, \"max_output_tokens\": 8}\ntext('ok');",
    )
    .expect("valid pragma");
    assert_eq!(parsed.yield_time_ms, Some(25));
    assert_eq!(parsed.max_output_tokens, Some(8));
    assert_eq!(parsed.code, "text('ok');");

    let error = parse_exec_source("// @exec: {\"unknown\": 1}\ntext('no');")
        .expect_err("unknown field should fail");
    assert!(error.contains("only supports"));
    assert!(parse_exec_source("   ").is_err());
}

#[tokio::test]
async fn exec_delegates_nested_calls_and_formats_completion() {
    let delegate = Arc::new(EchoDelegate::default());
    let session = InProcessCodeModeSession::with_delegate(delegate.clone());
    let nested = vec![echo_tool("echo")];
    let context = CodeModeToolContext::new(&session, "call-exec-1")
        .expect("context")
        .with_nested_tools(&nested);

    let output = execute_exec_tool(
        &ToolArguments::String(
            "const result = await tools.echo({ value: 7 }); text(result.answer);".to_string(),
        ),
        &context,
    )
    .await
    .expect("exec output");

    let text = text_parts(&output.content);
    assert!(text[0].starts_with("Script completed\nWall time"));
    assert_eq!(text[1], "7");
    assert_eq!(
        output.details.as_ref().expect("details")["status"],
        "completed"
    );
    assert_eq!(output.details.as_ref().expect("details")["success"], true);
    let calls = delegate.calls.lock().expect("calls lock");
    assert_eq!(calls.len(), 1);
    assert_eq!(calls[0].input, Some(json!({ "value": 7 })));
}

#[tokio::test]
async fn wait_returns_incremental_output_and_final_status() {
    let session = InProcessCodeModeSession::new();
    let context = CodeModeToolContext::new(&session, "call-exec-1").expect("context");
    let initial = execute_exec_tool(
        &ToolArguments::String(
            "// @exec: {\"yield_time_ms\": 60000}\ntext('before'); yield_control(); text('after');"
                .to_string(),
        ),
        &context,
    )
    .await
    .expect("initial exec");
    let initial_text = text_parts(&initial.content);
    assert!(initial_text[0].starts_with("Script running with cell ID 1"));
    assert_eq!(initial_text[1], "before");

    let wait = execute_wait_tool(
        &ToolArguments::Object(object(json!({
            "cell_id": "1",
            "yield_time_ms": 60000
        }))),
        &context,
    )
    .await
    .expect("wait output");
    let wait_text = text_parts(&wait.content);
    assert!(wait_text[0].starts_with("Script completed"));
    assert_eq!(wait_text[1], "after");
    assert!(!wait_text.iter().any(|text| text.contains("before")));
}

#[tokio::test]
async fn script_failure_is_a_failed_tool_output_not_an_adapter_error() {
    let session = InProcessCodeModeSession::new();
    let context = CodeModeToolContext::new(&session, "call-exec-1").expect("context");
    let output = execute_exec_tool(
        &ToolArguments::String("throw new Error('boom');".to_string()),
        &context,
    )
    .await
    .expect("script result should be returned");

    let text = text_parts(&output.content);
    assert!(text[0].starts_with("Script failed"));
    assert!(text[1].contains("Script error:"));
    assert!(text[1].contains("boom"));
    assert_eq!(output.details.as_ref().expect("details")["success"], false);
}

#[tokio::test]
async fn max_output_tokens_truncates_direct_script_text() {
    let session = InProcessCodeModeSession::new();
    let context = CodeModeToolContext::new(&session, "call-exec-1").expect("context");
    let output = execute_exec_tool(
        &ToolArguments::String(
            "// @exec: {\"max_output_tokens\": 2}\ntext('HEADxxxxxxxxxxxxxxxxxxxxxxxxTAIL');"
                .to_string(),
        ),
        &context,
    )
    .await
    .expect("truncated output");
    let text = text_parts(&output.content);
    assert!(text[1].contains("tokens truncated"));
    assert!(text[1].starts_with("HEAD"));
    assert!(text[1].ends_with("TAIL"));
}
