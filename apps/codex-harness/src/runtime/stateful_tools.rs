use std::{
    collections::HashMap,
    sync::{Arc, Weak},
    time::{SystemTime, UNIX_EPOCH},
};

use codex_code_mode_runtime::{
    CellId, CodeModeNestedToolCall, CodeModeSessionDelegate, CodeModeToolKind, NotificationFuture,
    ToolInvocationFuture,
};
use execution_runtime::{ExecutionRuntime, OperationContext};
use futures_util::future::try_join_all;
use llm_contracts::{
    AssistantContent, ContentPart, MessageId, TextContent, Timestamp, ToolArguments,
    ToolResultError, ToolResultMessage, ToolResultOutcome,
};
use serde_json::Value;
use tokio::sync::{Mutex, RwLock};
use tokio_util::sync::CancellationToken;
use uuid::Uuid;

use super::{
    CodexExecutionTarget, StatelessToolContext, StatelessToolExecutor,
    default_nested_tool_definitions,
};
use crate::persistence::CodexToolStateBackend;

/// Per-turn inputs used by both top-level code mode and its nested tools.
#[derive(Clone)]
pub struct CodexToolExecutionContext {
    pub agent_session_id: Uuid,
    pub runtime: Arc<dyn ExecutionRuntime>,
    pub execution: CodexExecutionTarget,
    pub operation: OperationContext,
}

/// Dispatches Codex's stateful tools and owns the stable delegates used by
/// live code-mode sessions.
#[derive(Clone)]
pub struct CodexToolExecutor {
    state: Arc<dyn CodexToolStateBackend>,
    stateless: Arc<StatelessToolExecutor>,
    delegates: Arc<Mutex<HashMap<Uuid, Weak<SessionNestedToolDelegate>>>>,
}

#[async_trait::async_trait]
pub trait CodexToolCallExecutor: Send + Sync {
    async fn execute_tool_calls(
        &self,
        content: &[AssistantContent],
        context: &CodexToolExecutionContext,
    ) -> Result<Vec<ToolResultMessage>, CodexToolDispatchError>;
}

impl CodexToolExecutor {
    #[must_use]
    pub fn new(state: Arc<dyn CodexToolStateBackend>) -> Self {
        Self {
            state,
            stateless: Arc::new(StatelessToolExecutor::new()),
            delegates: Arc::new(Mutex::new(HashMap::new())),
        }
    }

    /// Executes one assistant tool call. Tool failures are returned to the
    /// model; only a non-tool assistant item is a harness programming error.
    pub async fn execute_tool_call(
        &self,
        call: &AssistantContent,
        context: &CodexToolExecutionContext,
    ) -> Result<ToolResultMessage, CodexToolDispatchError> {
        let AssistantContent::ToolCall {
            name,
            arguments,
            tool_call_id,
        } = call
        else {
            return Err(CodexToolDispatchError::NotAToolCall);
        };

        if matches!(
            name.as_str(),
            tool_codex_apply_patch::TOOL_NAME | tool_codex_view_image::TOOL_NAME
        ) {
            return self
                .stateless
                .execute_tool_call(call, &stateless_context(context))
                .await
                .map_err(|_| CodexToolDispatchError::NotAToolCall);
        }

        let result = match name.as_str() {
            tool_codex_unified_exec::EXEC_COMMAND_TOOL_NAME
            | tool_codex_unified_exec::WRITE_STDIN_TOOL_NAME => {
                self.execute_unified(name, arguments, tool_call_id.as_str(), context)
                    .await
            }
            tool_codex_code_mode::EXEC_TOOL_NAME | tool_codex_code_mode::WAIT_TOOL_NAME => {
                self.execute_code_mode(name, arguments, tool_call_id.as_str(), context)
                    .await
            }
            _ => DispatchResult::error("unknown_tool", format!("Unknown tool `{name}`"), None),
        };

        Ok(result.into_message(name.clone(), tool_call_id.clone()))
    }

    /// Executes a batch concurrently while preserving assistant call order.
    pub async fn execute_tool_calls(
        &self,
        content: &[AssistantContent],
        context: &CodexToolExecutionContext,
    ) -> Result<Vec<ToolResultMessage>, CodexToolDispatchError> {
        try_join_all(
            content
                .iter()
                .filter(|item| matches!(item, AssistantContent::ToolCall { .. }))
                .map(|item| self.execute_tool_call(item, context)),
        )
        .await
    }

    async fn execute_unified(
        &self,
        name: &str,
        arguments: &ToolArguments,
        tool_call_id: &str,
        context: &CodexToolExecutionContext,
    ) -> DispatchResult {
        execute_unified(
            Arc::clone(&self.state),
            name,
            arguments,
            tool_call_id,
            context,
        )
        .await
    }

    async fn execute_code_mode(
        &self,
        name: &str,
        arguments: &ToolArguments,
        tool_call_id: &str,
        context: &CodexToolExecutionContext,
    ) -> DispatchResult {
        let delegate = self.delegate(context.agent_session_id).await;
        delegate.bind(context).await;
        let session = self
            .state
            .code_mode_session(context.agent_session_id, delegate.clone())
            .await;
        let nested_tools = default_nested_tool_definitions();
        let tool_context =
            match tool_codex_code_mode::CodeModeToolContext::new(session.as_ref(), tool_call_id) {
                Ok(context) => context.with_nested_tools(&nested_tools),
                Err(error) => return code_mode_error(error),
            };
        let output = match name {
            tool_codex_code_mode::EXEC_TOOL_NAME => {
                tool_codex_code_mode::execute_exec_tool(arguments, &tool_context).await
            }
            tool_codex_code_mode::WAIT_TOOL_NAME => {
                tool_codex_code_mode::execute_wait_tool(arguments, &tool_context).await
            }
            _ => unreachable!("caller filters code mode names"),
        };
        match output {
            Ok(mut output) => {
                if let Some(cell_id) = output
                    .details
                    .as_ref()
                    .and_then(|details| details.get("cell_id"))
                    .and_then(Value::as_str)
                {
                    let notifications = delegate.drain_notifications(cell_id).await;
                    if !notifications.is_empty() {
                        let mut content = notifications
                            .into_iter()
                            .map(text_content)
                            .collect::<Vec<_>>();
                        content.extend(output.content);
                        output.content = content;
                    }
                }
                let success = output
                    .details
                    .as_ref()
                    .and_then(|details| details.get("success"))
                    .and_then(Value::as_bool)
                    .unwrap_or(true);
                if success {
                    DispatchResult::success(output.content, output.details)
                } else {
                    DispatchResult::error_with_content(
                        "code_mode_script_error",
                        "Code-mode script failed",
                        output.content,
                        output.details,
                    )
                }
            }
            Err(error) => code_mode_error(error),
        }
    }

    async fn delegate(&self, agent_session_id: Uuid) -> Arc<SessionNestedToolDelegate> {
        let mut delegates = self.delegates.lock().await;
        delegates.retain(|_, delegate| delegate.strong_count() > 0);
        if let Some(delegate) = delegates.get(&agent_session_id).and_then(Weak::upgrade) {
            return delegate;
        }
        let delegate = Arc::new(SessionNestedToolDelegate::new(
            agent_session_id,
            Arc::clone(&self.state),
            Arc::clone(&self.stateless),
        ));
        delegates.insert(agent_session_id, Arc::downgrade(&delegate));
        delegate
    }
}

#[async_trait::async_trait]
impl CodexToolCallExecutor for CodexToolExecutor {
    async fn execute_tool_calls(
        &self,
        content: &[AssistantContent],
        context: &CodexToolExecutionContext,
    ) -> Result<Vec<ToolResultMessage>, CodexToolDispatchError> {
        CodexToolExecutor::execute_tool_calls(self, content, context).await
    }
}

#[derive(Debug, thiserror::Error)]
pub enum CodexToolDispatchError {
    #[error("assistant content is not a tool call")]
    NotAToolCall,
}

#[derive(Clone)]
struct NestedTurnBinding {
    runtime: Arc<dyn ExecutionRuntime>,
    execution: CodexExecutionTarget,
    operation: OperationContext,
}

struct SessionNestedToolDelegate {
    agent_session_id: Uuid,
    state: Arc<dyn CodexToolStateBackend>,
    stateless: Arc<StatelessToolExecutor>,
    binding: RwLock<Option<NestedTurnBinding>>,
    notifications: Mutex<HashMap<String, Vec<String>>>,
}

impl SessionNestedToolDelegate {
    fn new(
        agent_session_id: Uuid,
        state: Arc<dyn CodexToolStateBackend>,
        stateless: Arc<StatelessToolExecutor>,
    ) -> Self {
        Self {
            agent_session_id,
            state,
            stateless,
            binding: RwLock::new(None),
            notifications: Mutex::new(HashMap::new()),
        }
    }

    async fn bind(&self, context: &CodexToolExecutionContext) {
        *self.binding.write().await = Some(NestedTurnBinding {
            runtime: Arc::clone(&context.runtime),
            execution: context.execution.clone(),
            operation: context.operation.clone(),
        });
    }

    async fn drain_notifications(&self, cell_id: &str) -> Vec<String> {
        self.notifications
            .lock()
            .await
            .remove(cell_id)
            .unwrap_or_default()
    }

    async fn invoke(
        &self,
        invocation: CodeModeNestedToolCall,
        cancellation_token: CancellationToken,
    ) -> Result<Value, String> {
        if invocation.tool_name.namespace.is_some() {
            return Err(format!(
                "unknown nested tool `{}`",
                invocation.tool_name.name
            ));
        }
        if cancellation_token.is_cancelled() {
            return Err("code mode nested tool call cancelled".to_owned());
        }
        let Some(binding) = self.binding.read().await.clone() else {
            return Err("code mode nested tool dispatcher has no active turn".to_owned());
        };
        let operation = binding.operation.child();
        let cancelled_operation = operation.clone();
        let cancellation_watcher = tokio::spawn(async move {
            cancellation_token.cancelled().await;
            cancelled_operation.cancel();
        });
        let context = CodexToolExecutionContext {
            agent_session_id: self.agent_session_id,
            runtime: Arc::clone(&binding.runtime),
            execution: binding.execution,
            operation,
        };
        let result = match invocation.tool_name.name.as_str() {
            tool_codex_apply_patch::TOOL_NAME | tool_codex_view_image::TOOL_NAME => {
                self.stateless
                    .execute_nested_tool_call(&invocation, &stateless_context(&context))
                    .await
            }
            tool_codex_unified_exec::EXEC_COMMAND_TOOL_NAME
            | tool_codex_unified_exec::WRITE_STDIN_TOOL_NAME => {
                match nested_arguments(&invocation) {
                    Ok(arguments) => match execute_unified(
                        Arc::clone(&self.state),
                        &invocation.tool_name.name,
                        &arguments,
                        &invocation.runtime_tool_call_id,
                        &context,
                    )
                    .await
                    {
                        DispatchResult::Success { details, .. } => {
                            Ok(details.unwrap_or(Value::Null))
                        }
                        DispatchResult::Error { message, .. } => Err(message),
                    },
                    Err(error) => Err(error),
                }
            }
            name => Err(format!("unknown nested tool `{name}`")),
        };
        cancellation_watcher.abort();
        result
    }
}

impl CodeModeSessionDelegate for SessionNestedToolDelegate {
    fn invoke_tool<'a>(
        &'a self,
        invocation: CodeModeNestedToolCall,
        cancellation_token: CancellationToken,
    ) -> ToolInvocationFuture<'a> {
        Box::pin(self.invoke(invocation, cancellation_token))
    }

    fn notify<'a>(
        &'a self,
        _call_id: String,
        cell_id: CellId,
        text: String,
        cancellation_token: CancellationToken,
    ) -> NotificationFuture<'a> {
        Box::pin(async move {
            if cancellation_token.is_cancelled() {
                return Err("code mode notification cancelled".to_owned());
            }
            if !text.trim().is_empty() {
                self.notifications
                    .lock()
                    .await
                    .entry(cell_id.to_string())
                    .or_default()
                    .push(text);
            }
            Ok(())
        })
    }

    fn cell_closed(&self, _cell_id: &CellId) {}
}

async fn execute_unified(
    state: Arc<dyn CodexToolStateBackend>,
    name: &str,
    arguments: &ToolArguments,
    tool_call_id: &str,
    context: &CodexToolExecutionContext,
) -> DispatchResult {
    let sessions = state.exec_session_store(
        context.agent_session_id,
        context.execution.machine_id.clone(),
    );
    let tool_context = match tool_codex_unified_exec::CodexExecToolContext::new(
        context.runtime.as_ref(),
        &context.operation,
        sessions.as_ref(),
        context.execution.workspace_root_id.clone(),
        &context.execution.cwd,
        tool_call_id,
    ) {
        Ok(context) => context,
        Err(error) => return unified_error(error),
    };
    let output = match name {
        tool_codex_unified_exec::EXEC_COMMAND_TOOL_NAME => {
            tool_codex_unified_exec::execute_exec_command_tool(arguments, &tool_context).await
        }
        tool_codex_unified_exec::WRITE_STDIN_TOOL_NAME => {
            tool_codex_unified_exec::execute_write_stdin_tool(arguments, &tool_context).await
        }
        _ => unreachable!("caller filters unified exec names"),
    };
    match output {
        Ok(output) => DispatchResult::success(output.content, output.details),
        Err(error) => unified_error(error),
    }
}

fn nested_arguments(invocation: &CodeModeNestedToolCall) -> Result<ToolArguments, String> {
    match invocation.tool_kind {
        CodeModeToolKind::Function => match &invocation.input {
            None => Ok(ToolArguments::Object(serde_json::Map::new())),
            Some(Value::Object(arguments)) => Ok(ToolArguments::Object(arguments.clone())),
            Some(_) => Err(format!(
                "tool `{}` expects a JSON object for arguments",
                invocation.tool_name.name
            )),
        },
        CodeModeToolKind::Freeform => match &invocation.input {
            Some(Value::String(input)) => Ok(ToolArguments::String(input.clone())),
            _ => Err(format!(
                "tool `{}` expects a string input",
                invocation.tool_name.name
            )),
        },
    }
}

fn stateless_context(context: &CodexToolExecutionContext) -> StatelessToolContext<'_> {
    StatelessToolContext {
        runtime: context.runtime.as_ref(),
        execution: &context.execution,
        operation: &context.operation,
    }
}

fn unified_error(error: tool_codex_unified_exec::CodexExecToolError) -> DispatchResult {
    let (name, message, details) = error.into_parts();
    DispatchResult::error(name, message, details)
}

fn code_mode_error(error: tool_codex_code_mode::CodeModeToolError) -> DispatchResult {
    let (name, message, details) = error.into_parts();
    DispatchResult::error(name, message, details)
}

enum DispatchResult {
    Success {
        content: Vec<ContentPart>,
        details: Option<Value>,
    },
    Error {
        name: &'static str,
        message: String,
        content: Vec<ContentPart>,
        details: Option<Value>,
    },
}

impl DispatchResult {
    fn success(content: Vec<ContentPart>, details: Option<Value>) -> Self {
        Self::Success { content, details }
    }

    fn error(name: &'static str, message: String, details: Option<Value>) -> Self {
        Self::error_with_content(name, message.clone(), vec![text_content(message)], details)
    }

    fn error_with_content(
        name: &'static str,
        message: impl Into<String>,
        content: Vec<ContentPart>,
        details: Option<Value>,
    ) -> Self {
        Self::Error {
            name,
            message: message.into(),
            content,
            details,
        }
    }

    fn into_message(
        self,
        tool_name: String,
        tool_call_id: llm_contracts::ToolCallId,
    ) -> ToolResultMessage {
        let (content, details, outcome) = match self {
            Self::Success { content, details } => (content, details, ToolResultOutcome::Success),
            Self::Error {
                name,
                message,
                content,
                details,
            } => (
                content,
                details,
                ToolResultOutcome::Error {
                    error: ToolResultError {
                        message,
                        name: Some(name.to_owned()),
                    },
                },
            ),
        };
        ToolResultMessage {
            id: MessageId::new(format!("tool-result-{}", Uuid::now_v7()))
                .expect("UUID-backed tool result ID is valid"),
            tool_name,
            tool_call_id,
            content,
            details,
            timestamp: Timestamp(now_ms()),
            outcome,
        }
    }
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

#[cfg(test)]
mod tests {
    use std::{collections::HashMap, sync::Arc};

    use async_trait::async_trait;
    use codex_code_mode_runtime::{
        CodeModeSession, CodeModeSessionDelegate, InMemoryCodeModeStateStore,
    };
    use execution_contracts::{ExecutionId, MachineId, WorkspaceRootId};
    use execution_local::{LocalExecutionRuntime, LocalRuntimeConfig, LocalWorkspaceRoot};
    use execution_runtime::OperationContext;
    use llm_contracts::{
        AssistantContent, ContentPart, ToolArguments, ToolCallId, ToolResultOutcome,
    };
    use tokio::sync::Mutex;
    use tool_codex_unified_exec::{
        CodexExecSession, CodexExecSessionStore, CodexExecSessionStoreError,
    };
    use uuid::Uuid;

    use super::{CodexToolExecutionContext, CodexToolExecutor};
    use crate::{
        persistence::{CodeModeSessionFuture, CodexToolStateBackend, LiveCodeModeRegistry},
        runtime::CodexExecutionTarget,
    };

    #[tokio::test]
    async fn delegate_cache_does_not_retain_expired_code_mode_sessions() {
        let backend = Arc::new(InMemoryToolState::default());
        let executor = CodexToolExecutor::new(backend);
        let session_id = Uuid::now_v7();
        let first = executor.delegate(session_id).await;
        let first_weak = Arc::downgrade(&first);
        drop(first);
        assert!(first_weak.upgrade().is_none());

        let _replacement = executor.delegate(session_id).await;
        assert_eq!(executor.delegates.lock().await.len(), 1);
    }

    #[tokio::test]
    async fn unified_exec_session_is_available_to_a_later_turn() {
        let fixture = Fixture::new().await;
        let first_context = fixture.context(OperationContext::new());
        let first = fixture
            .executor
            .execute_tool_call(
                &call(
                    "exec_command",
                    object_arguments(serde_json::json!({
                        "cmd": "printf first; sleep 0.5; printf second",
                        "yield_time_ms": 250,
                        "login": false
                    })),
                    "exec-call",
                ),
                &first_context,
            )
            .await
            .expect("exec result");
        let session_id = first
            .details
            .as_ref()
            .and_then(|details| details.get("session_id"))
            .and_then(serde_json::Value::as_i64)
            .expect("running session ID");
        assert!(text(&first.content).contains("first"));

        let second_context = fixture.context(OperationContext::new());
        let second = fixture
            .executor
            .execute_tool_call(
                &call(
                    "write_stdin",
                    object_arguments(serde_json::json!({
                        "session_id": session_id,
                        "yield_time_ms": 5_000
                    })),
                    "write-call",
                ),
                &second_context,
            )
            .await
            .expect("write result");

        assert_eq!(second.outcome, ToolResultOutcome::Success);
        assert_eq!(
            second.details.as_ref().expect("details")["output"],
            "second"
        );
        assert!(second.details.as_ref().expect("details")["session_id"].is_null());
    }

    #[tokio::test]
    async fn yielded_cell_resumes_nested_tools_with_the_next_turn_binding() {
        let fixture = Fixture::new().await;
        let first = fixture
            .executor
            .execute_tool_call(
                &call(
                    "exec",
                    ToolArguments::String(
                        r#"notify("notice"); text("before"); yield_control();
const result = await tools.exec_command({cmd: "printf nested", login: false});
text(result.output);
store("answer", 42);"#
                            .to_owned(),
                    ),
                    "code-exec",
                ),
                &fixture.context(OperationContext::new()),
            )
            .await
            .expect("initial exec result");
        assert_eq!(first.outcome, ToolResultOutcome::Success);
        assert_eq!(
            first.details.as_ref().expect("details")["status"],
            "running"
        );
        let first_text = text(&first.content);
        assert!(first_text.contains("notice"));
        assert!(first_text.contains("before"));
        assert!(!first_text.contains("nested"));

        let wait = fixture
            .executor
            .execute_tool_call(
                &call(
                    "wait",
                    object_arguments(serde_json::json!({
                        "cell_id": first.details.as_ref().expect("details")["cell_id"],
                        "yield_time_ms": 5_000
                    })),
                    "code-wait",
                ),
                &fixture.context(OperationContext::new()),
            )
            .await
            .expect("wait result");
        assert_eq!(wait.outcome, ToolResultOutcome::Success);
        assert_eq!(
            wait.details.as_ref().expect("details")["status"],
            "completed"
        );
        assert!(text(&wait.content).contains("nested"));

        let load = fixture
            .executor
            .execute_tool_call(
                &call(
                    "exec",
                    ToolArguments::String(r#"text(load("answer"));"#.to_owned()),
                    "code-load",
                ),
                &fixture.context(OperationContext::new()),
            )
            .await
            .expect("load result");
        assert_eq!(load.details.as_ref().expect("details")["cell_id"], "2");
        assert!(text(&load.content).contains("42"));
    }

    #[tokio::test]
    async fn script_failure_is_a_model_visible_failed_tool_result() {
        let fixture = Fixture::new().await;
        let result = fixture
            .executor
            .execute_tool_call(
                &call(
                    "exec",
                    ToolArguments::String("throw new Error('boom');".to_owned()),
                    "failed-exec",
                ),
                &fixture.context(OperationContext::new()),
            )
            .await
            .expect("script result");

        let ToolResultOutcome::Error { error } = result.outcome else {
            panic!("script failure should be a failed tool result")
        };
        assert_eq!(error.name.as_deref(), Some("code_mode_script_error"));
        assert!(text(&result.content).contains("boom"));
    }

    struct Fixture {
        _directory: tempfile::TempDir,
        runtime: Arc<LocalExecutionRuntime>,
        execution: CodexExecutionTarget,
        agent_session_id: Uuid,
        executor: CodexToolExecutor,
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
            let machine_id = MachineId::new("machine").expect("machine ID");
            let workspace_root_id = WorkspaceRootId::new("root").expect("root ID");
            let runtime = Arc::new(
                LocalExecutionRuntime::new(LocalRuntimeConfig {
                    machine_id: machine_id.clone(),
                    name: "Codex stateful tool tests".to_owned(),
                    state_directory: directory.path().join("state"),
                    workspace_roots: vec![LocalWorkspaceRoot {
                        id: workspace_root_id.clone(),
                        name: "workspace".to_owned(),
                        path: workspace,
                        read_only: false,
                    }],
                    native_grants: Vec::new(),
                })
                .await
                .expect("local runtime"),
            );
            let backend = Arc::new(InMemoryToolState::default());
            Self {
                _directory: directory,
                runtime,
                execution: CodexExecutionTarget {
                    machine_id,
                    workspace_root_id,
                    cwd: "project".to_owned(),
                },
                agent_session_id: Uuid::now_v7(),
                executor: CodexToolExecutor::new(backend),
            }
        }

        fn context(&self, operation: OperationContext) -> CodexToolExecutionContext {
            CodexToolExecutionContext {
                agent_session_id: self.agent_session_id,
                runtime: self.runtime.clone(),
                execution: self.execution.clone(),
                operation,
            }
        }
    }

    #[derive(Default)]
    struct InMemoryToolState {
        exec: Arc<InMemoryExecSessionStore>,
        code_state: Arc<InMemoryCodeModeStateStore>,
        live: LiveCodeModeRegistry,
    }

    impl CodexToolStateBackend for InMemoryToolState {
        fn exec_session_store(
            &self,
            _agent_session_id: Uuid,
            _machine_id: MachineId,
        ) -> Arc<dyn CodexExecSessionStore> {
            self.exec.clone()
        }

        fn code_mode_session<'a>(
            &'a self,
            agent_session_id: Uuid,
            delegate: Arc<dyn CodeModeSessionDelegate>,
        ) -> CodeModeSessionFuture<'a> {
            Box::pin(async move {
                let state = self.code_state.clone();
                let session = self
                    .live
                    .get_or_create(agent_session_id, delegate, state)
                    .await;
                session as Arc<dyn CodeModeSession>
            })
        }
    }

    #[derive(Default)]
    struct InMemoryExecSessionStore {
        sessions: Mutex<HashMap<i32, Arc<CodexExecSession>>>,
        next_id: Mutex<i32>,
    }

    #[async_trait]
    impl CodexExecSessionStore for InMemoryExecSessionStore {
        async fn insert(
            &self,
            execution_id: ExecutionId,
            tty: bool,
        ) -> Result<Arc<CodexExecSession>, CodexExecSessionStoreError> {
            let mut next_id = self.next_id.lock().await;
            *next_id = next_id.saturating_add(1).max(1);
            let session = Arc::new(CodexExecSession::new(*next_id, execution_id, tty)?);
            self.sessions.lock().await.insert(*next_id, session.clone());
            Ok(session)
        }

        async fn get(
            &self,
            session_id: i32,
        ) -> Result<Option<Arc<CodexExecSession>>, CodexExecSessionStoreError> {
            Ok(self.sessions.lock().await.get(&session_id).cloned())
        }

        async fn update_last_sequence(
            &self,
            _session_id: i32,
            _execution_id: &ExecutionId,
            _last_sequence: u64,
        ) -> Result<(), CodexExecSessionStoreError> {
            Ok(())
        }

        async fn remove(
            &self,
            session_id: i32,
            execution_id: &ExecutionId,
        ) -> Result<(), CodexExecSessionStoreError> {
            let mut sessions = self.sessions.lock().await;
            if sessions
                .get(&session_id)
                .is_some_and(|session| session.execution_id() == execution_id)
            {
                sessions.remove(&session_id);
            }
            Ok(())
        }
    }

    fn call(name: &str, arguments: ToolArguments, call_id: &str) -> AssistantContent {
        AssistantContent::ToolCall {
            name: name.to_owned(),
            arguments,
            tool_call_id: ToolCallId::new(call_id).expect("tool call ID"),
        }
    }

    fn object_arguments(value: serde_json::Value) -> ToolArguments {
        ToolArguments::Object(value.as_object().expect("object arguments").clone())
    }

    fn text(content: &[ContentPart]) -> String {
        content
            .iter()
            .filter_map(|part| match part {
                ContentPart::Text(text) => Some(text.content.as_str()),
                ContentPart::Image(_) => None,
            })
            .collect::<Vec<_>>()
            .join("\n")
    }
}
