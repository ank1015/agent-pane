use std::sync::{Arc, Mutex};
use std::time::Duration;

use codex_code_mode_runtime::{
    CellId, CodeModeNestedToolCall, CodeModeSessionDelegate, CodeModeToolKind, ExecuteRequest,
    FunctionCallOutputContentItem, ImageDetail, InMemoryCodeModeStateStore,
    InProcessCodeModeSession, NotificationFuture, RuntimeResponse, ToolDefinition,
    ToolInvocationFuture, ToolName, WaitOutcome, WaitRequest,
};
use serde_json::{Value, json};
use tokio_util::sync::CancellationToken;

#[derive(Default)]
struct RecordingDelegate {
    calls: Mutex<Vec<CodeModeNestedToolCall>>,
    notifications: Mutex<Vec<(String, CellId, String)>>,
    closed_cells: Mutex<Vec<CellId>>,
}

impl CodeModeSessionDelegate for RecordingDelegate {
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
            match invocation.tool_name.name.as_str() {
                "echo" => Ok(invocation.input.unwrap_or(Value::Null)),
                name => Err(format!("unknown nested tool `{name}`")),
            }
        })
    }

    fn notify<'a>(
        &'a self,
        call_id: String,
        cell_id: CellId,
        text: String,
        _cancellation_token: CancellationToken,
    ) -> NotificationFuture<'a> {
        self.notifications
            .lock()
            .expect("notifications lock")
            .push((call_id, cell_id, text));
        Box::pin(async { Ok(()) })
    }

    fn cell_closed(&self, cell_id: &CellId) {
        self.closed_cells
            .lock()
            .expect("closed cells lock")
            .push(cell_id.clone());
    }
}

fn request(source: &str) -> ExecuteRequest {
    ExecuteRequest {
        tool_call_id: "outer-call-1".to_string(),
        enabled_tools: Vec::new(),
        source: source.to_string(),
        yield_time_ms: Some(1_000),
        max_output_tokens: None,
    }
}

fn echo_definition() -> ToolDefinition {
    ToolDefinition {
        name: "echo".to_string(),
        tool_name: ToolName::plain("echo"),
        description: "Returns its input.".to_string(),
        kind: CodeModeToolKind::Function,
        input_schema: None,
        output_schema: None,
    }
}

async fn execute(session: &InProcessCodeModeSession, request: ExecuteRequest) -> RuntimeResponse {
    session
        .execute(request)
        .await
        .expect("start cell")
        .initial_response()
        .await
        .expect("initial response")
}

#[tokio::test]
async fn evaluates_helpers_and_exit_in_a_fresh_isolate() {
    let session = InProcessCodeModeSession::new();
    let response = execute(
        &session,
        request(
            r#"
text("before");
text({ answer: 42 });
image({ image_url: "data:image/png;base64,AA==", detail: "original" });
exit();
text("after");
"#,
        ),
    )
    .await;

    assert_eq!(
        response,
        RuntimeResponse::Result {
            cell_id: CellId::new("1"),
            content_items: vec![
                FunctionCallOutputContentItem::InputText {
                    text: "before".to_string(),
                },
                FunctionCallOutputContentItem::InputText {
                    text: r#"{"answer":42}"#.to_string(),
                },
                FunctionCallOutputContentItem::InputImage {
                    image_url: "data:image/png;base64,AA==".to_string(),
                    detail: Some(ImageDetail::Original),
                },
            ],
            error_text: None,
        }
    );
}

#[tokio::test]
async fn delegates_nested_tools_and_resolves_their_promises() {
    let delegate = Arc::new(RecordingDelegate::default());
    let session = InProcessCodeModeSession::with_delegate(delegate.clone());
    let response = execute(
        &session,
        ExecuteRequest {
            enabled_tools: vec![echo_definition()],
            source: r#"
const result = await tools.echo({ value: 7 });
text(result.value);
notify("nested call completed");
"#
            .to_string(),
            ..request("")
        },
    )
    .await;

    assert_eq!(
        response,
        RuntimeResponse::Result {
            cell_id: CellId::new("1"),
            content_items: vec![FunctionCallOutputContentItem::InputText {
                text: "7".to_string(),
            }],
            error_text: None,
        }
    );
    assert_eq!(delegate.calls.lock().expect("calls lock").len(), 1);
    assert_eq!(
        delegate.calls.lock().expect("calls lock")[0].input,
        Some(json!({ "value": 7 }))
    );
    assert_eq!(
        delegate
            .notifications
            .lock()
            .expect("notifications lock")
            .as_slice(),
        &[(
            "outer-call-1".to_string(),
            CellId::new("1"),
            "nested call completed".to_string(),
        )]
    );
}

#[tokio::test]
async fn wait_returns_only_output_since_the_previous_observation() {
    let session = InProcessCodeModeSession::new();
    let started = session
        .execute(ExecuteRequest {
            source: r#"text("before"); yield_control(); text("after");"#.to_string(),
            yield_time_ms: Some(60_000),
            ..request("")
        })
        .await
        .expect("start cell");

    assert_eq!(
        started.initial_response().await.expect("yield response"),
        RuntimeResponse::Yielded {
            cell_id: CellId::new("1"),
            content_items: vec![FunctionCallOutputContentItem::InputText {
                text: "before".to_string(),
            }],
        }
    );
    assert_eq!(
        session
            .wait(WaitRequest {
                cell_id: CellId::new("1"),
                yield_time_ms: 60_000,
            })
            .await
            .expect("wait response"),
        WaitOutcome::LiveCell(RuntimeResponse::Result {
            cell_id: CellId::new("1"),
            content_items: vec![FunctionCallOutputContentItem::InputText {
                text: "after".to_string(),
            }],
            error_text: None,
        })
    );
}

#[tokio::test]
async fn store_and_load_are_shared_within_only_one_session() {
    let first = InProcessCodeModeSession::new();
    let second = InProcessCodeModeSession::new();

    let write = execute(&first, request(r#"store("key", { value: 9 });"#)).await;
    assert!(matches!(
        write,
        RuntimeResponse::Result {
            error_text: None,
            ..
        }
    ));

    let same_session = execute(&first, request(r#"text(load("key").value);"#)).await;
    let other_session = execute(&second, request(r#"text(String(load("key")));"#)).await;

    assert_eq!(
        same_session,
        RuntimeResponse::Result {
            cell_id: CellId::new("2"),
            content_items: vec![FunctionCallOutputContentItem::InputText {
                text: "9".to_string(),
            }],
            error_text: None,
        }
    );
    assert_eq!(
        other_session,
        RuntimeResponse::Result {
            cell_id: CellId::new("1"),
            content_items: vec![FunctionCallOutputContentItem::InputText {
                text: "undefined".to_string(),
            }],
            error_text: None,
        }
    );
}

#[tokio::test]
async fn external_state_survives_runtime_recreation() {
    let state = Arc::new(InMemoryCodeModeStateStore::default());
    let first = InProcessCodeModeSession::with_delegate_and_state_store(
        Arc::new(RecordingDelegate::default()),
        state.clone(),
    );

    let write = execute(&first, request(r#"store("key", { value: 9 });"#)).await;
    assert!(matches!(
        write,
        RuntimeResponse::Result {
            cell_id,
            error_text: None,
            ..
        } if cell_id == CellId::new("1")
    ));
    first.shutdown().await.expect("first session shutdown");

    let second = InProcessCodeModeSession::with_delegate_and_state_store(
        Arc::new(RecordingDelegate::default()),
        state,
    );
    assert_eq!(
        execute(&second, request(r#"text(load("key").value);"#)).await,
        RuntimeResponse::Result {
            cell_id: CellId::new("2"),
            content_items: vec![FunctionCallOutputContentItem::InputText {
                text: "9".to_owned(),
            }],
            error_text: None,
        }
    );
}

#[tokio::test]
async fn terminate_interrupts_synchronous_javascript_and_closes_the_cell() {
    let delegate = Arc::new(RecordingDelegate::default());
    let session = InProcessCodeModeSession::with_delegate(delegate.clone());
    let started = session
        .execute(ExecuteRequest {
            source: "while (true) {}".to_string(),
            yield_time_ms: Some(1),
            ..request("")
        })
        .await
        .expect("start cell");

    assert_eq!(
        tokio::time::timeout(Duration::from_secs(2), started.initial_response())
            .await
            .expect("initial response timeout")
            .expect("initial response"),
        RuntimeResponse::Yielded {
            cell_id: CellId::new("1"),
            content_items: Vec::new(),
        }
    );
    assert_eq!(
        tokio::time::timeout(Duration::from_secs(2), session.terminate(CellId::new("1")))
            .await
            .expect("termination timeout")
            .expect("termination response"),
        WaitOutcome::LiveCell(RuntimeResponse::Terminated {
            cell_id: CellId::new("1"),
            content_items: Vec::new(),
        })
    );
    assert_eq!(
        delegate
            .closed_cells
            .lock()
            .expect("closed cells lock")
            .as_slice(),
        &[CellId::new("1")]
    );
}
