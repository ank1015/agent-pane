use std::{
    collections::HashMap,
    sync::{Arc, Weak},
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};

use codex_code_mode_runtime::{
    CellId, CodeModeNestedToolCall, CodeModeSessionDelegate, CodeModeToolKind, NotificationFuture,
    ToolInvocationFuture,
};
use execution_runtime::{ExecutionRuntime, OperationContext};
use futures_util::future::try_join_all;
use llm_contracts::{
    AssistantContent, ContentPart, MessageId, ProviderId, SearchCommands, SearchInput,
    SearchRequest, SearchRequestOptions, TextContent, Timestamp, ToolArguments, ToolResultError,
    ToolResultMessage, ToolResultOutcome,
};
use serde_json::Value;
use tokio::sync::{Mutex, RwLock};
use tokio_util::sync::CancellationToken;
use uuid::Uuid;

use super::{
    CodexExecutionTarget, StatelessToolContext, StatelessToolExecutor,
    code_mode_nested_tool_definitions, tool_admission::ToolAdmissionGate,
};
use crate::{clients::LlmGatewayClient, persistence::CodexToolStateBackend};

/// Request-scoped inputs used by Codex's standalone `web.run` extension.
#[derive(Clone)]
pub struct CodexWebSearchExecutionContext {
    pub provider: ProviderId,
    pub model: String,
    pub account_id: Option<Uuid>,
    pub input: Option<SearchInput>,
}

/// Per-turn inputs used by both top-level code mode and its nested tools.
#[derive(Clone)]
pub struct CodexToolExecutionContext {
    pub agent_session_id: Uuid,
    pub runtime: Arc<dyn ExecutionRuntime>,
    pub execution: CodexExecutionTarget,
    pub operation: OperationContext,
    pub web_search: Option<CodexWebSearchExecutionContext>,
}

/// Dispatches Codex's stateful tools and owns the stable delegates used by
/// live code-mode sessions.
#[derive(Clone)]
pub struct CodexToolExecutor {
    state: Arc<dyn CodexToolStateBackend>,
    stateless: Arc<StatelessToolExecutor>,
    search: Option<LlmGatewayClient>,
    delegates: Arc<Mutex<HashMap<Uuid, Weak<SessionNestedToolDelegate>>>>,
}

/// Codex uses separate request-scoped runtimes for top-level and code-mode
/// nested calls. Keeping both gates in one batch makes that split explicit.
#[derive(Default)]
struct ToolAdmissionBatch {
    top_level: ToolAdmissionGate,
    nested: ToolAdmissionGate,
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
            search: None,
            delegates: Arc::new(Mutex::new(HashMap::new())),
        }
    }

    #[must_use]
    pub fn with_llm_gateway(
        state: Arc<dyn CodexToolStateBackend>,
        gateway: LlmGatewayClient,
    ) -> Self {
        Self {
            state,
            stateless: Arc::new(StatelessToolExecutor::new()),
            search: Some(gateway),
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
        let admission = ToolAdmissionBatch::default();
        self.execute_tool_call_in_batch(call, context, &admission)
            .await
    }

    async fn execute_tool_call_in_batch(
        &self,
        call: &AssistantContent,
        context: &CodexToolExecutionContext,
        admission: &ToolAdmissionBatch,
    ) -> Result<ToolResultMessage, CodexToolDispatchError> {
        let AssistantContent::ToolCall {
            name,
            arguments,
            tool_call_id,
        } = call
        else {
            return Err(CodexToolDispatchError::NotAToolCall);
        };
        let started = Instant::now();
        let execution = async {
            let _admission = admission.top_level.acquire(name).await;

            if matches!(
                name.as_str(),
                tool_codex_apply_patch::TOOL_NAME | tool_codex_view_image::TOOL_NAME
            ) {
                return self
                    .stateless
                    .execute_tool_call_admitted(call, &stateless_context(context))
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
                    self.execute_code_mode(
                        name,
                        arguments,
                        tool_call_id.as_str(),
                        context,
                        &admission.nested,
                    )
                    .await
                }
                _ => DispatchResult::error("unknown_tool", format!("Unknown tool `{name}`"), None),
            };

            Ok(result.into_message(name.clone(), tool_call_id.clone()))
        };
        tokio::pin!(execution);

        tokio::select! {
            biased;
            result = &mut execution => match result {
                Ok(result)
                    if context.operation.is_cancelled()
                        && is_cancellation_origin(&result) =>
                {
                    Ok(user_aborted_tool_result(
                        name,
                        tool_call_id.clone(),
                        started.elapsed(),
                    ))
                }
                result => result,
            },
            () = context.operation.cancelled() => {
                Ok(user_aborted_tool_result(name, tool_call_id.clone(), started.elapsed()))
            }
        }
    }

    /// Executes a batch concurrently while preserving assistant call order.
    pub async fn execute_tool_calls(
        &self,
        content: &[AssistantContent],
        context: &CodexToolExecutionContext,
    ) -> Result<Vec<ToolResultMessage>, CodexToolDispatchError> {
        let admission = ToolAdmissionBatch::default();
        try_join_all(
            content
                .iter()
                .filter(|item| matches!(item, AssistantContent::ToolCall { .. }))
                .map(|item| self.execute_tool_call_in_batch(item, context, &admission)),
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
        nested_admission: &ToolAdmissionGate,
    ) -> DispatchResult {
        let delegate = self.delegate(context.agent_session_id).await;
        delegate.bind(context, nested_admission.clone()).await;
        let session = self
            .state
            .code_mode_session(context.agent_session_id, delegate.clone())
            .await;
        let nested_tools = code_mode_nested_tool_definitions(context.web_search.is_some());
        let tool_context =
            match tool_codex_code_mode::CodeModeToolContext::new(session.as_ref(), tool_call_id) {
                Ok(context) => context.with_runtime_nested_tools(&nested_tools),
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
            self.search.clone(),
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
    admission: ToolAdmissionGate,
    web_search: Option<CodexWebSearchExecutionContext>,
}

struct SessionNestedToolDelegate {
    agent_session_id: Uuid,
    state: Arc<dyn CodexToolStateBackend>,
    stateless: Arc<StatelessToolExecutor>,
    search: Option<LlmGatewayClient>,
    binding: RwLock<Option<NestedTurnBinding>>,
    notifications: Mutex<HashMap<String, Vec<String>>>,
}

impl SessionNestedToolDelegate {
    fn new(
        agent_session_id: Uuid,
        state: Arc<dyn CodexToolStateBackend>,
        stateless: Arc<StatelessToolExecutor>,
        search: Option<LlmGatewayClient>,
    ) -> Self {
        Self {
            agent_session_id,
            state,
            stateless,
            search,
            binding: RwLock::new(None),
            notifications: Mutex::new(HashMap::new()),
        }
    }

    async fn bind(&self, context: &CodexToolExecutionContext, admission: ToolAdmissionGate) {
        *self.binding.write().await = Some(NestedTurnBinding {
            runtime: Arc::clone(&context.runtime),
            execution: context.execution.clone(),
            operation: context.operation.clone(),
            admission,
            web_search: context.web_search.clone(),
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
        if cancellation_token.is_cancelled() {
            return Err("code mode nested tool call cancelled".to_owned());
        }
        let Some(binding) = self.binding.read().await.clone() else {
            return Err("code mode nested tool dispatcher has no active turn".to_owned());
        };
        let admission = binding.admission.clone();
        let admission_future = async {
            if is_web_run(&invocation) {
                admission.acquire("web.run").await
            } else if invocation.tool_name.namespace.is_some() {
                admission.acquire_exclusive().await
            } else {
                admission.acquire(&invocation.tool_name.name).await
            }
        };
        tokio::pin!(admission_future);
        let _admission = tokio::select! {
            biased;
            admission = &mut admission_future => admission,
            () = cancellation_token.cancelled() => {
                return Err("code mode nested tool call cancelled".to_owned());
            }
        };
        if invocation.tool_name.namespace.is_some() && !is_web_run(&invocation) {
            return Err(format!(
                "unknown nested tool `{}`",
                invocation.tool_name.name
            ));
        }
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
            web_search: binding.web_search,
        };
        let result = match (
            invocation.tool_name.namespace.as_deref(),
            invocation.tool_name.name.as_str(),
        ) {
            (Some(tool_codex_web_search::WEB_NAMESPACE), tool_codex_web_search::RUN_TOOL_NAME) => {
                self.execute_web_search(&invocation, &context).await
            }
            (None, tool_codex_apply_patch::TOOL_NAME | tool_codex_view_image::TOOL_NAME) => {
                self.stateless
                    .execute_nested_tool_call_admitted(&invocation, &stateless_context(&context))
                    .await
            }
            (
                None,
                tool_codex_unified_exec::EXEC_COMMAND_TOOL_NAME
                | tool_codex_unified_exec::WRITE_STDIN_TOOL_NAME,
            ) => match nested_arguments(&invocation) {
                Ok(arguments) => match execute_unified(
                    Arc::clone(&self.state),
                    &invocation.tool_name.name,
                    &arguments,
                    &invocation.runtime_tool_call_id,
                    &context,
                )
                .await
                {
                    DispatchResult::Success { details, .. } => Ok(details.unwrap_or(Value::Null)),
                    DispatchResult::Error { message, .. } => Err(message),
                },
                Err(error) => Err(error),
            },
            _ => Err(format!(
                "unknown nested tool `{}`",
                display_nested_tool_name(&invocation)
            )),
        };
        cancellation_watcher.abort();
        result
    }

    async fn execute_web_search(
        &self,
        invocation: &CodeModeNestedToolCall,
        context: &CodexToolExecutionContext,
    ) -> Result<Value, String> {
        let Some(search) = &self.search else {
            return Err("web search transport is unavailable".to_owned());
        };
        let Some(web) = &context.web_search else {
            return Err("web search is disabled for this run".to_owned());
        };
        let input = invocation
            .input
            .clone()
            .unwrap_or_else(|| Value::Object(serde_json::Map::new()));
        let commands = serde_json::from_value::<SearchCommands>(input)
            .map_err(|error| format!("invalid web.run arguments: {error}"))?;
        let request = SearchRequest {
            id: context.agent_session_id.to_string(),
            model: web.model.clone(),
            reasoning: None,
            input: web.input.clone(),
            commands: Some(commands),
            settings: Some(tool_codex_web_search::default_search_settings()),
            max_output_tokens: Some(tool_codex_web_search::DEFAULT_SEARCH_MAX_OUTPUT_TOKENS),
        };
        let response = search
            .search(
                web.account_id,
                &web.provider,
                &request,
                &SearchRequestOptions::default(),
                &context.operation,
            )
            .await
            .map_err(|error| format!("web search request failed: {error}"))?;
        Ok(Value::String(response.output))
    }
}

fn is_web_run(invocation: &CodeModeNestedToolCall) -> bool {
    invocation.tool_name.namespace.as_deref() == Some(tool_codex_web_search::WEB_NAMESPACE)
        && invocation.tool_name.name == tool_codex_web_search::RUN_TOOL_NAME
}

fn display_nested_tool_name(invocation: &CodeModeNestedToolCall) -> String {
    invocation.tool_name.namespace.as_ref().map_or_else(
        || invocation.tool_name.name.clone(),
        |namespace| format!("{namespace}.{}", invocation.tool_name.name),
    )
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

fn user_aborted_tool_result(
    tool_name: &str,
    tool_call_id: llm_contracts::ToolCallId,
    elapsed: Duration,
) -> ToolResultMessage {
    let seconds = elapsed.as_secs_f32().max(0.1);
    let message = if tool_name == tool_codex_unified_exec::EXEC_COMMAND_TOOL_NAME {
        format!("Wall time: {seconds:.1} seconds\naborted by user")
    } else {
        format!("aborted by user after {seconds:.1}s")
    };
    ToolResultMessage {
        id: MessageId::new(format!("codex-user-aborted-tool-{tool_call_id}"))
            .expect("tool-call-derived result ID is valid"),
        tool_name: tool_name.to_owned(),
        tool_call_id,
        content: vec![text_content(message)],
        details: None,
        timestamp: Timestamp(now_ms()),
        // Codex emits aborted outputs without a `success: false` wire flag.
        // Success also prevents provider adapters from decorating the payload.
        outcome: ToolResultOutcome::Success,
    }
}

fn is_cancellation_origin(result: &ToolResultMessage) -> bool {
    let ToolResultOutcome::Error { error } = &result.outcome else {
        return false;
    };
    if error.name.as_deref() == Some("cancelled") {
        return true;
    }
    error.name.as_deref() == Some("execution_error")
        && result
            .details
            .as_ref()
            .and_then(|details| details.get("code"))
            .and_then(Value::as_str)
            == Some("cancelled")
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
    use std::{
        collections::HashMap,
        path::Path,
        sync::{Arc, Mutex as StdMutex},
        time::Duration,
    };

    use async_trait::async_trait;
    use axum::{Json, Router, extract::State, routing::post};
    use codex_code_mode_runtime::{
        CellId, CodeModeNestedToolCall, CodeModeSession, CodeModeSessionDelegate, CodeModeToolKind,
        InMemoryCodeModeStateStore, ToolName,
    };
    use execution_contracts::{ExecutionId, MachineId, WorkspaceRootId};
    use execution_local::{LocalExecutionRuntime, LocalRuntimeConfig, LocalWorkspaceRoot};
    use execution_runtime::OperationContext;
    use llm_contracts::{
        AssistantContent, ContentPart, ProviderId, SearchInput, ToolArguments, ToolCallId,
        ToolResultOutcome,
    };
    use serde_json::Value as JsonValue;
    use tokio::net::TcpListener;
    use tokio::sync::Mutex;
    use tokio_util::sync::CancellationToken;
    use tool_codex_unified_exec::{
        CodexExecSession, CodexExecSessionStore, CodexExecSessionStoreError,
    };
    use uuid::Uuid;

    use super::{
        CodexToolExecutionContext, CodexToolExecutor, CodexWebSearchExecutionContext,
        DispatchResult, ToolAdmissionBatch, ToolAdmissionGate, is_cancellation_origin,
        user_aborted_tool_result,
    };
    use crate::{
        clients::LlmGatewayClient,
        config::LlmGatewayServiceConfig,
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

    #[tokio::test]
    async fn web_run_uses_codex_search_request_defaults_and_returns_only_output() {
        let captured = Arc::new(StdMutex::new(None));
        let app = Router::new()
            .route("/v1/search", post(search_response))
            .with_state(captured.clone());
        let listener = TcpListener::bind("127.0.0.1:0").await.expect("bind server");
        let address = listener.local_addr().expect("server address");
        tokio::spawn(async move {
            axum::serve(listener, app).await.expect("serve gateway");
        });
        let gateway = LlmGatewayClient::new(LlmGatewayServiceConfig {
            base_url: format!("http://{address}").parse().expect("gateway URL"),
            request_timeout: Duration::from_secs(5),
        })
        .expect("gateway client");
        let fixture = Fixture::new().await;
        let executor =
            CodexToolExecutor::with_llm_gateway(Arc::new(InMemoryToolState::default()), gateway);
        let account_id = Uuid::now_v7();
        let mut context = fixture.context(OperationContext::new());
        context.web_search = Some(CodexWebSearchExecutionContext {
            provider: ProviderId::new("openai").expect("provider"),
            model: "gpt-5.6-sol".to_owned(),
            account_id: Some(account_id),
            input: Some(SearchInput::Items(vec![serde_json::json!({
                "type": "message",
                "role": "user",
                "content": [{"type": "input_text", "text": "current question"}]
            })])),
        });

        let result = executor
            .execute_tool_call(
                &call(
                    "exec",
                    ToolArguments::String(
                        r#"const result = await tools.web__run({
    search_query: [{q: "Codex web.run"}],
    response_length: "short"
});
text(result);"#
                            .to_owned(),
                    ),
                    "web-code-mode",
                ),
                &context,
            )
            .await
            .expect("code-mode result");

        assert_eq!(result.outcome, ToolResultOutcome::Success);
        let output = text(&result.content);
        assert!(output.contains("web answer"));
        assert!(!output.contains("computer_initialize_state"));
        let request = captured.lock().expect("capture lock").clone().unwrap();
        assert_eq!(request["account_id"], account_id.to_string());
        assert_eq!(request["provider"], "openai");
        assert_eq!(
            request["request"]["id"],
            fixture.agent_session_id.to_string()
        );
        assert_eq!(request["request"]["model"], "gpt-5.6-sol");
        assert!(request["request"].get("reasoning").is_none());
        assert_eq!(request["request"]["input"][0]["role"], "user");
        assert_eq!(
            request["request"]["commands"],
            serde_json::json!({
                "search_query": [{"q": "Codex web.run"}],
                "response_length": "short"
            })
        );
        assert_eq!(
            request["request"]["settings"],
            serde_json::json!({
                "allowed_callers": ["direct"],
                "external_web_access": false
            })
        );
        assert_eq!(request["request"]["max_output_tokens"], 2_500);
        assert_eq!(request["request_options"], serde_json::json!({}));
    }

    async fn search_response(
        State(captured): State<Arc<StdMutex<Option<JsonValue>>>>,
        Json(request): Json<JsonValue>,
    ) -> Json<JsonValue> {
        *captured.lock().expect("capture lock") = Some(request);
        Json(serde_json::json!({
            "request_id": Uuid::now_v7(),
            "account_id": Uuid::now_v7(),
            "response": {
                "encrypted_output": "opaque",
                "output": "web answer",
                "results": [{"type": "computer_initialize_state"}]
            }
        }))
    }

    #[tokio::test]
    async fn request_scoped_top_level_gate_is_separate_from_nested_worker_gate() {
        let fixture = Fixture::new().await;
        let project = fixture.project_path();
        let calls = vec![
            call(
                "exec",
                ToolArguments::String(
                    r#"const result = await tools.exec_command({
    cmd: "touch nested-started; while [ ! -f nested-release ]; do sleep 0.01; done",
    login: false,
    yield_time_ms: 5000
});
text(result.output);"#
                        .to_owned(),
                ),
                "exclusive-code-mode",
            ),
            call(
                "exec_command",
                object_arguments(serde_json::json!({
                    "cmd": "touch same-batch-direct",
                    "login": false
                })),
                "same-batch-parallel",
            ),
        ];
        let executor = fixture.executor.clone();
        let context = fixture.context(OperationContext::new());
        let first_batch =
            tokio::spawn(async move { executor.execute_tool_calls(&calls, &context).await });

        assert!(
            wait_for_paths(&[project.join("nested-started")], Duration::from_secs(2)).await,
            "nested exec must use a separate gate from its exclusive outer exec"
        );
        tokio::time::sleep(Duration::from_millis(50)).await;
        assert!(
            !project.join("same-batch-direct").exists(),
            "exclusive top-level exec must block a parallel-safe top-level call in its batch"
        );

        let other_calls = [call(
            "exec_command",
            object_arguments(serde_json::json!({
                "cmd": "touch other-batch-direct",
                "login": false
            })),
            "other-batch-parallel",
        )];
        let other_context = fixture.context(OperationContext::new());
        let other_batch = fixture
            .executor
            .execute_tool_calls(&other_calls, &other_context);
        tokio::time::timeout(Duration::from_secs(2), other_batch)
            .await
            .expect("a different sampling batch must not share top-level admission")
            .expect("other batch result");
        assert!(project.join("other-batch-direct").exists());
        assert!(!project.join("same-batch-direct").exists());

        tokio::fs::write(project.join("nested-release"), b"")
            .await
            .expect("release nested command");
        let results = tokio::time::timeout(Duration::from_secs(5), first_batch)
            .await
            .expect("first batch should finish after release")
            .expect("first batch task")
            .expect("first batch results");
        assert_eq!(
            results
                .iter()
                .map(|result| result.tool_call_id.as_str())
                .collect::<Vec<_>>(),
            ["exclusive-code-mode", "same-batch-parallel"]
        );
        assert!(project.join("same-batch-direct").exists());
    }

    #[tokio::test]
    async fn nested_parallel_safe_calls_share_reader_admission() {
        let fixture = Fixture::new().await;
        let project = fixture.project_path();
        let executor = fixture.executor.clone();
        let context = fixture.context(OperationContext::new());
        let call = call(
            "exec",
            ToolArguments::String(
                r#"const first = tools.exec_command({
    cmd: "touch nested-first; while [ ! -f nested-readers-release ]; do sleep 0.01; done",
    login: false,
    yield_time_ms: 5000
});
const second = tools.exec_command({
    cmd: "touch nested-second; while [ ! -f nested-readers-release ]; do sleep 0.01; done",
    login: false,
    yield_time_ms: 5000
});
const results = await Promise.all([first, second]);
text(results.map(result => result.output).join(""));"#
                    .to_owned(),
            ),
            "nested-readers",
        );
        let running =
            tokio::spawn(async move { executor.execute_tool_call(&call, &context).await });

        let both_started = wait_for_paths(
            &[project.join("nested-first"), project.join("nested-second")],
            Duration::from_secs(2),
        )
        .await;
        tokio::fs::write(project.join("nested-readers-release"), b"")
            .await
            .expect("release nested readers");
        let result = tokio::time::timeout(Duration::from_secs(5), running)
            .await
            .expect("nested reader script should finish")
            .expect("nested reader task")
            .expect("nested reader result");

        assert!(both_started, "parallel-safe nested calls should overlap");
        assert_eq!(result.outcome, ToolResultOutcome::Success);
    }

    #[tokio::test]
    async fn running_unified_cancellation_uses_scheduler_abort_result() {
        let fixture = Fixture::new().await;
        let project = fixture.project_path();
        let operation = OperationContext::new();
        let executor = fixture.executor.clone();
        let context = fixture.context(operation.clone());
        let call = call(
            "exec_command",
            object_arguments(serde_json::json!({
                "cmd": "touch running-unified-started; while [ ! -f running-unified-release ]; do sleep 0.01; done",
                "login": false,
                "yield_time_ms": 5_000
            })),
            "running-unified",
        );
        let running =
            tokio::spawn(async move { executor.execute_tool_call(&call, &context).await });

        assert!(
            wait_for_paths(
                &[project.join("running-unified-started")],
                Duration::from_secs(2),
            )
            .await,
            "command should be running before cancellation"
        );
        operation.cancel();
        let result = tokio::time::timeout(Duration::from_secs(1), running)
            .await
            .expect("running command should observe cancellation")
            .expect("running command task")
            .expect("running command result");
        tokio::fs::write(project.join("running-unified-release"), b"")
            .await
            .expect("release running command");

        assert_eq!(result.outcome, ToolResultOutcome::Success);
        let output = text(&result.content);
        assert!(output.starts_with("Wall time: "));
        assert!(output.ends_with(" seconds\naborted by user"));
        assert!(!output.contains("unified exec interaction was cancelled"));
        assert!(result.details.is_none());
    }

    #[tokio::test]
    async fn mixed_batch_retains_completion_and_aborts_only_running_call_in_order() {
        let fixture = Fixture::new().await;
        let project = fixture.project_path();
        let operation = OperationContext::new();
        let executor = fixture.executor.clone();
        let context = fixture.context(operation.clone());
        let calls = vec![
            call(
                "exec_command",
                object_arguments(serde_json::json!({
                    "cmd": "printf completed; touch mixed-completed",
                    "login": false,
                    "yield_time_ms": 5_000
                })),
                "completed-call",
            ),
            call(
                "exec_command",
                object_arguments(serde_json::json!({
                    "cmd": "sleep 0.3; touch mixed-running; while [ ! -f mixed-release ]; do sleep 0.01; done",
                    "login": false,
                    "yield_time_ms": 5_000
                })),
                "running-call",
            ),
        ];
        let running =
            tokio::spawn(async move { executor.execute_tool_calls(&calls, &context).await });

        assert!(
            wait_for_paths(
                &[
                    project.join("mixed-completed"),
                    project.join("mixed-running"),
                ],
                Duration::from_secs(2),
            )
            .await,
            "one call should finish before the other is cancelled"
        );
        operation.cancel();
        let results = tokio::time::timeout(Duration::from_secs(1), running)
            .await
            .expect("mixed batch should observe cancellation")
            .expect("mixed batch task")
            .expect("mixed batch results");
        tokio::fs::write(project.join("mixed-release"), b"")
            .await
            .expect("release running command");

        assert_eq!(
            results
                .iter()
                .map(|result| result.tool_call_id.as_str())
                .collect::<Vec<_>>(),
            ["completed-call", "running-call"]
        );
        assert_eq!(results[0].outcome, ToolResultOutcome::Success);
        assert!(text(&results[0].content).contains("completed"));
        assert!(!text(&results[0].content).contains("aborted by user"));
        assert_eq!(results[1].outcome, ToolResultOutcome::Success);
        assert!(text(&results[1].content).starts_with("Wall time: "));
        assert!(text(&results[1].content).ends_with(" seconds\naborted by user"));
    }

    #[tokio::test]
    async fn namespaced_known_tool_uses_exclusive_nested_admission() {
        let fixture = Fixture::new().await;
        let admission = ToolAdmissionGate::default();
        let delegate = fixture.executor.delegate(fixture.agent_session_id).await;
        let context = fixture.context(OperationContext::new());
        delegate.bind(&context, admission.clone()).await;
        let reader = admission.acquire("exec_command").await;
        let invocation = CodeModeNestedToolCall {
            cell_id: CellId::new("cell-1"),
            runtime_tool_call_id: "namespaced-call".to_owned(),
            tool_name: ToolName {
                name: "exec_command".to_owned(),
                namespace: Some("unexpected".to_owned()),
            },
            tool_kind: CodeModeToolKind::Function,
            input: Some(serde_json::json!({"cmd": "printf should-not-run"})),
        };
        let invocation = delegate.invoke(invocation, CancellationToken::new());
        tokio::pin!(invocation);

        assert!(
            tokio::time::timeout(Duration::from_millis(30), &mut invocation)
                .await
                .is_err(),
            "a namespaced unknown must wait for exclusive admission"
        );
        drop(reader);
        let error = tokio::time::timeout(Duration::from_secs(1), &mut invocation)
            .await
            .expect("unknown call should finish after exclusive admission")
            .expect_err("namespaced call must remain unknown");
        assert_eq!(error, "unknown nested tool `exec_command`");
    }

    #[tokio::test]
    async fn queued_call_is_aborted_without_waiting_for_admission() {
        let fixture = Fixture::new().await;
        let project = fixture.project_path();
        let operation = OperationContext::new();
        let context = fixture.context(operation.clone());
        let admission = Arc::new(ToolAdmissionBatch::default());
        let task_admission = Arc::clone(&admission);
        let exclusive = admission.top_level.acquire("exec").await;
        let executor = fixture.executor.clone();
        let call = call(
            "exec_command",
            object_arguments(serde_json::json!({
                "cmd": "touch should-not-run-after-cancellation",
                "login": false
            })),
            "queued-exec",
        );
        let running = tokio::spawn(async move {
            executor
                .execute_tool_call_in_batch(&call, &context, &task_admission)
                .await
        });

        tokio::time::sleep(Duration::from_millis(120)).await;
        operation.cancel();
        let result = tokio::time::timeout(Duration::from_secs(1), running)
            .await
            .expect("queued call should stop when its operation is cancelled")
            .expect("queued call task")
            .expect("queued call result");

        assert_eq!(result.outcome, ToolResultOutcome::Success);
        let output = text(&result.content);
        assert!(output.starts_with("Wall time: "));
        assert!(output.ends_with(" seconds\naborted by user"));
        assert!(result.details.is_none());
        drop(exclusive);
        tokio::time::sleep(Duration::from_millis(20)).await;
        assert!(!project.join("should-not-run-after-cancellation").exists());
    }

    #[test]
    fn aborted_results_match_codex_text_and_portable_status() {
        let exec = user_aborted_tool_result(
            "exec_command",
            ToolCallId::new("exec-id").expect("tool call ID"),
            Duration::from_millis(2_340),
        );
        assert_eq!(
            text(&exec.content),
            "Wall time: 2.3 seconds\naborted by user"
        );
        assert_eq!(exec.outcome, ToolResultOutcome::Success);
        assert!(exec.details.is_none());

        let wait = user_aborted_tool_result(
            "wait",
            ToolCallId::new("wait-id").expect("tool call ID"),
            Duration::from_millis(2_340),
        );
        assert_eq!(text(&wait.content), "aborted by user after 2.3s");
        assert_eq!(wait.outcome, ToolResultOutcome::Success);
        assert!(wait.details.is_none());

        let execution_cancelled = DispatchResult::error(
            "execution_error",
            "transport cancelled".to_owned(),
            Some(serde_json::json!({"code": "cancelled"})),
        )
        .into_message(
            "apply_patch".to_owned(),
            ToolCallId::new("patch-id").expect("tool call ID"),
        );
        assert!(is_cancellation_origin(&execution_cancelled));

        let genuine_error = DispatchResult::error(
            "execution_error",
            "not found".to_owned(),
            Some(serde_json::json!({"code": "not_found"})),
        )
        .into_message(
            "view_image".to_owned(),
            ToolCallId::new("image-id").expect("tool call ID"),
        );
        assert!(!is_cancellation_origin(&genuine_error));
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
                web_search: None,
            }
        }

        fn project_path(&self) -> std::path::PathBuf {
            self._directory.path().join("workspace/project")
        }
    }

    async fn wait_for_paths(paths: &[impl AsRef<Path>], timeout: Duration) -> bool {
        let deadline = tokio::time::Instant::now() + timeout;
        loop {
            if paths.iter().all(|path| path.as_ref().exists()) {
                return true;
            }
            if tokio::time::Instant::now() >= deadline {
                return false;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
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
