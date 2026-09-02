use std::{
    sync::{
        Arc, Mutex,
        atomic::{AtomicU64, AtomicUsize, Ordering},
    },
    time::Duration,
};

use agent_contracts::{
    AppendSessionMessages, HARNESS_PROTOCOL_VERSION, HarnessOperation, SessionMessage,
    SessionMessageDelivery, SessionMessageOrigin, SessionMessagePage, SessionMessagesAppended,
    TurnRequested,
};
use agent_harness_sdk::{ActiveTurn, AgentClient, TurnOutcome};
use axum::{
    Json, Router,
    extract::{Path, State},
    http::StatusCode,
    response::{IntoResponse, Response},
    routing::get,
};
use chrono::Utc;
use codex_harness::{
    clients::{ExecutionResolutionError, MachineRuntimeResolver},
    config::AgentServiceConfig,
    runtime::{
        CODEX_ENVIRONMENT_MESSAGE_TAG, CODEX_PRIMARY_CALL_STARTED_TAG, CodexCompletionClient,
        CodexModelCallError, CodexRuntime, CodexToolCallExecutor, CodexToolDispatchError,
        CodexToolExecutionContext,
    },
};
use execution_contracts::{MachineId, WorkspaceRootId};
use execution_local::{LocalExecutionRuntime, LocalRuntimeConfig, LocalWorkspaceRoot};
use execution_runtime::{ExecutionRuntime, OperationContext};
use llm_contracts::{
    AssistantContent, AssistantMessage, ContentPart, LlmRequest, Message, MessageId, TextContent,
    Timestamp, ToolResultMessage, ToolResultOutcome,
};
use serde_json::json;
use tokio::sync::Notify;
use url::Url;
use uuid::Uuid;

#[tokio::test]
async fn calls_one_primary_model_and_commits_a_terminal_assistant() {
    let agent = Arc::new(AgentState::new(vec![]));
    let model = Arc::new(FakeModel::new(vec![assistant("done", &[])]));
    let tools = Arc::new(FakeTools::default());
    let fixture = RuntimeFixture::new(model.clone(), tools).await;
    let turn = active_turn(&agent).await;

    let outcome = fixture.runtime.execute(&turn).await.expect("turn outcome");

    let TurnOutcome::Command(HarnessOperation::Complete { final_message_id }) = outcome else {
        panic!("terminal assistant should complete the run")
    };
    assert_eq!(model.calls.load(Ordering::Acquire), 1);
    let requests = model.requests.lock().expect("model requests");
    assert_eq!(requests.len(), 1);
    assert_eq!(requests[0].model.id.as_str(), "gpt-5.6-sol");
    assert_eq!(requests[0].tools.len(), 2);
    assert!(requests[0].messages.iter().any(|message| {
        matches!(message, Message::User(user) if user.id.as_str().starts_with("codex-environment-"))
    }));
    drop(requests);

    let messages = agent.messages.lock().expect("Agent messages");
    assert_eq!(messages.len(), 4);
    assert!(matches!(
        &messages[1].message,
        Message::Custom(custom)
            if custom.tag.as_deref() == Some(CODEX_ENVIRONMENT_MESSAGE_TAG)
                && custom.content["text"]
                    .as_str()
                    .is_some_and(|text| text.contains("<environment_context>"))
    ));
    assert!(matches!(
        &messages[2].message,
        Message::Custom(custom)
            if custom.tag.as_deref() == Some(CODEX_PRIMARY_CALL_STARTED_TAG)
    ));
    assert_eq!(messages[3].session_message_id, final_message_id);
    assert!(matches!(messages[3].message, Message::Assistant(_)));
}

#[tokio::test]
async fn refuses_to_resample_after_a_committed_primary_start_marker() {
    let marker = message(json!({
        "role": "custom",
        "id": "primary-started",
        "content": {},
        "tag": CODEX_PRIMARY_CALL_STARTED_TAG,
        "timestamp": 1
    }));
    let agent = Arc::new(AgentState::new(vec![(marker, true)]));
    let model = Arc::new(FakeModel::new(vec![assistant("unused", &[])]));
    let fixture = RuntimeFixture::new(model.clone(), Arc::new(FakeTools::default())).await;
    let turn = active_turn(&agent).await;

    let outcome = fixture.runtime.execute(&turn).await.expect("turn outcome");

    let TurnOutcome::Command(HarnessOperation::Fail { failure }) = outcome else {
        panic!("interrupted primary call should fail deterministically")
    };
    assert_eq!(failure["code"], "primary_model_call_interrupted");
    assert_eq!(model.calls.load(Ordering::Acquire), 0);
}

#[tokio::test]
async fn does_not_reexecute_a_historical_tool_missing_its_result() {
    let assistant = message(json!({
        "role": "assistant",
        "id": "assistant-tools",
        "model": {"provider": "openai", "id": "gpt-5.6-sol"},
        "duration_ms": 1,
        "native_message": {"output": []},
        "content": [{
            "type": "tool_call",
            "name": "view_image",
            "arguments": {"path": "/workspace/image.png"},
            "tool_call_id": "call-1"
        }],
        "stop_reason": "tool_use",
        "timestamp": 1
    }));
    let agent = Arc::new(AgentState::new(vec![(assistant, true)]));
    let model = Arc::new(FakeModel::new(Vec::new()));
    let tools = Arc::new(FakeTools::default());
    let fixture = RuntimeFixture::new(model.clone(), tools.clone()).await;
    let turn = active_turn(&agent).await;

    let outcome = fixture.runtime.execute(&turn).await.expect("turn outcome");

    assert_eq!(outcome, TurnOutcome::Command(HarnessOperation::Continue));
    assert_eq!(model.calls.load(Ordering::Acquire), 0);
    assert_eq!(tools.calls.load(Ordering::Acquire), 0);
    let messages = agent.messages.lock().expect("Agent messages");
    assert_eq!(messages.len(), 2, "recovery is prompt-only like Codex");
}

#[tokio::test]
async fn cancellation_commits_codex_aborted_results_before_the_turn_exits() {
    let agent = Arc::new(AgentState::new(vec![]));
    let model = Arc::new(FakeModel::new(vec![assistant(
        "assistant-tools",
        &[("call-exec", "exec_command"), ("call-wait", "wait")],
    )]));
    let tools = Arc::new(BlockingTools::default());
    let fixture = RuntimeFixture::new(model, tools.clone()).await;
    let turn = active_turn(&agent).await;
    let started = tools.started.notified();
    tokio::pin!(started);
    let execution = fixture.runtime.execute(&turn);
    tokio::pin!(execution);

    tokio::select! {
        () = &mut started => {
            agent.aborted.store(true, Ordering::Release);
            turn.operation().cancel();
        },
        result = &mut execution => panic!("turn completed before cancellation: {result:?}"),
    }
    let outcome = tokio::time::timeout(Duration::from_secs(2), execution)
        .await
        .expect("cancelled turn should return promptly")
        .expect("turn outcome");
    assert_eq!(outcome, TurnOutcome::Cancelled);
    assert_eq!(
        agent.cancellation_appends.load(Ordering::Acquire),
        1,
        "runtime must retry through the narrowly fenced abort-cleanup append"
    );

    let messages = agent.messages.lock().expect("Agent messages");
    let results = messages
        .iter()
        .filter_map(|message| match &message.message {
            Message::ToolResult(result) => Some(result),
            _ => None,
        })
        .collect::<Vec<_>>();
    assert_eq!(results.len(), 2);
    assert!(matches!(results[0].outcome, ToolResultOutcome::Success));
    let ContentPart::Text(exec_text) = &results[0].content[0] else {
        panic!("exec cancellation must be text")
    };
    assert!(exec_text.content.starts_with("Wall time: "));
    assert!(exec_text.content.ends_with("aborted by user"));
    let ContentPart::Text(wait_text) = &results[1].content[0] else {
        panic!("wait cancellation must be text")
    };
    assert!(wait_text.content.starts_with("aborted by user after "));
    assert!(wait_text.content.ends_with('s'));
}

struct RuntimeFixture {
    runtime: CodexRuntime,
    _directory: tempfile::TempDir,
}

impl RuntimeFixture {
    async fn new(model: Arc<FakeModel>, tools: Arc<dyn CodexToolCallExecutor>) -> Self {
        let directory = tempfile::tempdir().expect("temporary directory");
        let workspace = directory.path().join("workspace");
        tokio::fs::create_dir_all(workspace.join("project"))
            .await
            .expect("workspace");
        let runtime = LocalExecutionRuntime::new(LocalRuntimeConfig {
            machine_id: MachineId::new("machine-a").expect("machine ID"),
            name: "Codex runtime test".to_owned(),
            state_directory: directory.path().join("state"),
            workspace_roots: vec![LocalWorkspaceRoot {
                id: WorkspaceRootId::new("root").expect("workspace root ID"),
                name: "workspace".to_owned(),
                path: workspace,
                read_only: false,
            }],
            native_grants: Vec::new(),
        })
        .await
        .expect("local runtime");
        let execution = Arc::new(FakeExecution {
            runtime: Arc::new(runtime),
        });
        Self {
            runtime: CodexRuntime::new(model, execution, tools, "UTC"),
            _directory: directory,
        }
    }
}

struct FakeExecution {
    runtime: Arc<LocalExecutionRuntime>,
}

#[async_trait::async_trait]
impl MachineRuntimeResolver for FakeExecution {
    async fn machine(
        &self,
        machine_id: &MachineId,
    ) -> Result<Arc<dyn ExecutionRuntime>, ExecutionResolutionError> {
        assert_eq!(machine_id.as_str(), "machine-a");
        Ok(self.runtime.clone())
    }
}

struct FakeModel {
    calls: AtomicUsize,
    responses: Mutex<Vec<AssistantMessage>>,
    requests: Mutex<Vec<LlmRequest>>,
}

impl FakeModel {
    fn new(mut responses: Vec<AssistantMessage>) -> Self {
        responses.reverse();
        Self {
            calls: AtomicUsize::new(0),
            responses: Mutex::new(responses),
            requests: Mutex::new(Vec::new()),
        }
    }
}

#[async_trait::async_trait]
impl CodexCompletionClient for FakeModel {
    async fn complete(
        &self,
        _account_id: Option<Uuid>,
        request: &LlmRequest,
        _operation: &OperationContext,
    ) -> Result<AssistantMessage, CodexModelCallError> {
        self.calls.fetch_add(1, Ordering::AcqRel);
        self.requests
            .lock()
            .expect("model requests")
            .push(request.clone());
        Ok(self
            .responses
            .lock()
            .expect("model responses")
            .pop()
            .expect("queued response"))
    }
}

#[derive(Default)]
struct FakeTools {
    calls: AtomicUsize,
}

#[async_trait::async_trait]
impl CodexToolCallExecutor for FakeTools {
    async fn execute_tool_calls(
        &self,
        content: &[AssistantContent],
        _context: &CodexToolExecutionContext,
    ) -> Result<Vec<ToolResultMessage>, CodexToolDispatchError> {
        self.calls.fetch_add(1, Ordering::AcqRel);
        Ok(content
            .iter()
            .map(|content| {
                let AssistantContent::ToolCall {
                    name, tool_call_id, ..
                } = content
                else {
                    panic!("tool call")
                };
                ToolResultMessage {
                    id: MessageId::new(format!("result-{tool_call_id}")).expect("tool result ID"),
                    tool_name: name.clone(),
                    tool_call_id: tool_call_id.clone(),
                    content: vec![ContentPart::Text(TextContent {
                        content: "ok".to_owned(),
                        metadata: None,
                    })],
                    details: None,
                    timestamp: Timestamp(1),
                    outcome: ToolResultOutcome::Success,
                }
            })
            .collect())
    }
}

#[derive(Default)]
struct BlockingTools {
    started: Notify,
}

#[async_trait::async_trait]
impl CodexToolCallExecutor for BlockingTools {
    async fn execute_tool_calls(
        &self,
        content: &[AssistantContent],
        context: &CodexToolExecutionContext,
    ) -> Result<Vec<ToolResultMessage>, CodexToolDispatchError> {
        self.started.notify_one();
        context.operation.cancelled().await;
        Ok(content
            .iter()
            .map(|content| {
                let AssistantContent::ToolCall {
                    name, tool_call_id, ..
                } = content
                else {
                    panic!("tool call")
                };
                let text = if name == "exec_command" {
                    "Wall time: 0.1 seconds\naborted by user"
                } else {
                    "aborted by user after 0.1s"
                };
                ToolResultMessage {
                    id: MessageId::new(format!("cancelled-{tool_call_id}"))
                        .expect("tool result ID"),
                    tool_name: name.clone(),
                    tool_call_id: tool_call_id.clone(),
                    content: vec![ContentPart::Text(TextContent {
                        content: text.to_owned(),
                        metadata: None,
                    })],
                    details: None,
                    timestamp: Timestamp(1),
                    outcome: ToolResultOutcome::Success,
                }
            })
            .collect())
    }
}

struct AgentState {
    run_id: Uuid,
    session_id: Uuid,
    revision: AtomicU64,
    aborted: std::sync::atomic::AtomicBool,
    cancellation_appends: AtomicUsize,
    messages: Mutex<Vec<SessionMessage>>,
}

impl AgentState {
    fn new(additional: Vec<(Message, bool)>) -> Self {
        let run_id = Uuid::now_v7();
        let session_id = Uuid::now_v7();
        let mut messages = vec![session_message(session_id, run_id, 1, user(), false)];
        for (index, (message, current_turn)) in additional.into_iter().enumerate() {
            messages.push(session_message(
                session_id,
                run_id,
                u64::try_from(index).unwrap() + 2,
                message,
                current_turn,
            ));
        }
        let revision = u64::try_from(messages.len()).unwrap();
        Self {
            run_id,
            session_id,
            revision: AtomicU64::new(revision),
            aborted: std::sync::atomic::AtomicBool::new(false),
            cancellation_appends: AtomicUsize::new(0),
            messages: Mutex::new(messages),
        }
    }

    fn request(&self) -> TurnRequested {
        TurnRequested {
            protocol_version: HARNESS_PROTOCOL_VERSION,
            event_id: Uuid::now_v7(),
            emitted_at: Utc::now(),
            run_id: self.run_id,
            session_id: self.session_id,
            turn_number: 1,
            max_turns: 100,
            expected_state_version: 7,
            harness_id: "codex".to_owned(),
            harness_slug: "codex".to_owned(),
            harness_revision_id: "codex-test".to_owned(),
            resolved_config: json!({
                "provider": "openai",
                "model_id": "gpt-5.6-sol",
                "reasoning_level": "low",
                "execution": {
                    "machine_id": "machine-a",
                    "workspace_root_id": "root",
                    "cwd": "project"
                }
            })
            .as_object()
            .unwrap()
            .clone(),
            current_session_revision: self.revision.load(Ordering::Acquire),
            resume: None,
        }
    }
}

async fn active_turn(state: &Arc<AgentState>) -> ActiveTurn {
    let app = Router::new()
        .route(
            "/v1/harness/runs/{run_id}/messages",
            get(fetch_messages).post(append_messages),
        )
        .with_state(state.clone());
    let client = AgentClient::new(AgentServiceConfig {
        base_url: serve(app).await,
        harness_token: "harness-secret".to_owned(),
        request_timeout: Duration::from_secs(2),
    })
    .expect("Agent client");
    ActiveTurn::new(client, state.request(), OperationContext::new())
}

async fn fetch_messages(
    State(state): State<Arc<AgentState>>,
    Path(run_id): Path<Uuid>,
) -> Json<SessionMessagePage> {
    assert_eq!(run_id, state.run_id);
    Json(SessionMessagePage {
        items: state.messages.lock().expect("Agent messages").clone(),
        next_after_revision: None,
    })
}

async fn append_messages(
    State(state): State<Arc<AgentState>>,
    Path(run_id): Path<Uuid>,
    Json(command): Json<AppendSessionMessages>,
) -> Response {
    assert_eq!(run_id, state.run_id);
    if state.aborted.load(Ordering::Acquire) && !command.after_cancellation {
        return (
            StatusCode::CONFLICT,
            Json(json!({
                "error": {
                    "code": "run_state_conflict",
                    "message": "run was aborted"
                }
            })),
        )
            .into_response();
    }
    if command.after_cancellation {
        state.cancellation_appends.fetch_add(1, Ordering::AcqRel);
    }
    let mut revision = state.revision.load(Ordering::Acquire);
    assert_eq!(command.expected_session_revision, revision);
    let items = command
        .messages
        .into_iter()
        .map(|new| {
            revision += 1;
            let mut message =
                session_message(state.session_id, state.run_id, revision, new.message, true);
            message.session_message_id = new.session_message_id;
            message
        })
        .collect::<Vec<_>>();
    state.revision.store(revision, Ordering::Release);
    state
        .messages
        .lock()
        .expect("Agent messages")
        .extend(items.clone());
    Json(SessionMessagesAppended {
        items,
        current_session_revision: revision,
    })
    .into_response()
}

fn session_message(
    session_id: Uuid,
    run_id: Uuid,
    revision: u64,
    message: Message,
    current_turn: bool,
) -> SessionMessage {
    SessionMessage {
        session_message_id: Uuid::now_v7(),
        session_id,
        revision,
        message,
        origin: if current_turn {
            SessionMessageOrigin::Harness
        } else {
            SessionMessageOrigin::External
        },
        delivery: SessionMessageDelivery::Immediate,
        run_id: current_turn.then_some(run_id),
        turn_number: current_turn.then_some(1),
        created_at: Utc::now(),
        committed_at: Utc::now(),
    }
}

fn user() -> Message {
    message(json!({
        "role": "user",
        "id": "user-1",
        "timestamp": 1,
        "content": [{"type": "text", "content": "Inspect the project."}]
    }))
}

fn assistant(id: &str, calls: &[(&str, &str)]) -> AssistantMessage {
    let content = if calls.is_empty() {
        json!([{"type": "response", "response": {"content": "complete"}}])
    } else {
        serde_json::Value::Array(
            calls
                .iter()
                .map(|(call_id, name)| {
                    json!({
                        "type": "tool_call",
                        "name": name,
                        "arguments": {},
                        "tool_call_id": call_id
                    })
                })
                .collect(),
        )
    };
    serde_json::from_value(json!({
        "id": id,
        "model": {"provider": "openai", "id": "gpt-5.6-sol"},
        "duration_ms": 1,
        "native_message": {"output": []},
        "content": content,
        "stop_reason": if calls.is_empty() { "stop" } else { "tool_use" },
        "timestamp": 1
    }))
    .expect("assistant")
}

fn message(value: serde_json::Value) -> Message {
    serde_json::from_value(value).expect("message")
}

async fn serve(app: Router) -> Url {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind test server");
    let address = listener.local_addr().expect("test server address");
    tokio::spawn(async move {
        axum::serve(listener, app).await.expect("serve test app");
    });
    format!("http://{address}").parse().expect("server URL")
}
