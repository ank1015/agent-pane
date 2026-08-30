use std::{sync::Arc, time::Duration};

use agent_contracts::{HarnessOperation, NewRunMessage, SessionMessage, SessionMessagesAppended};
use agent_harness_sdk::{ActiveTurn, ActiveTurnError, HarnessRuntime, TurnOutcome};
use chrono::Local;
use execution_runtime::{ExecutionRuntime, OperationContext};
use llm_contracts::{
    AssistantContent, AssistantMessage, ContentPart, CustomMessage, JsonObject, Message, MessageId,
    StopReason, TextContent, Timestamp, ToolResultError, ToolResultMessage, ToolResultOutcome,
};
use serde_json::json;
use uuid::Uuid;

use crate::{
    clients::{
        ExecutionClient, ExecutionResolutionError, LlmGatewayClientError, MachineRuntimeResolver,
    },
    config::HarnessConfig,
    persistence::CodexToolState,
};

use super::{
    CODEX_COMPACTION_MESSAGE_TAG, CODEX_CONTEXT_OVERFLOW_TAG, CODEX_PRIMARY_CALL_STARTED_TAG,
    CodexCompletionClient, CodexEnvironmentError, CodexEnvironmentSnapshot, CodexHarnessConfig,
    CodexHarnessConfigError, CodexModelCallError, CodexModelClient, CodexModelFailureKind,
    CodexToolCallExecutor, CodexToolDispatchError, CodexToolExecutionContext, CodexToolExecutor,
    CompactionError, CompactionPlan, ContextFormationError, ResumePlan, TranscriptError,
    checkpoint_from_response, form_compaction_request, form_main_request, plan_compaction,
    plan_turn,
};

const AGENT_MAX_RETRIES: u32 = 3;
const AGENT_RETRY_BASE: Duration = Duration::from_millis(250);

/// Complete Codex runtime for one Agent broker turn.
///
/// The runtime may make one auxiliary compaction request and one primary model
/// request. It never makes a second primary request for the same delivered
/// `(run_id, turn_number)`.
#[derive(Clone)]
pub struct CodexRuntime {
    model: Arc<dyn CodexCompletionClient>,
    execution: Arc<dyn MachineRuntimeResolver>,
    tools: Arc<dyn CodexToolCallExecutor>,
    timezone: Arc<str>,
}

impl CodexRuntime {
    pub fn from_config(
        config: &HarnessConfig,
        tool_state: Arc<CodexToolState>,
    ) -> Result<Self, CodexRuntimeBuildError> {
        let model = CodexModelClient::from_config(config.llm_gateway.clone())?;
        let execution = ExecutionClient::new(config.execution_gateway.clone())?;
        let tools = CodexToolExecutor::new(tool_state);
        Ok(Self::new(
            Arc::new(model),
            Arc::new(execution),
            Arc::new(tools),
            config.timezone.clone(),
        ))
    }

    #[must_use]
    pub fn new(
        model: Arc<dyn CodexCompletionClient>,
        execution: Arc<dyn MachineRuntimeResolver>,
        tools: Arc<dyn CodexToolCallExecutor>,
        timezone: impl Into<Arc<str>>,
    ) -> Self {
        Self {
            model,
            execution,
            tools,
            timezone: timezone.into(),
        }
    }

    pub async fn execute(&self, turn: &ActiveTurn) -> Result<TurnOutcome, CodexRuntimeError> {
        match self.execute_turn(turn).await {
            Ok(decision) => Ok(decision.finish()),
            Err(error) => self.handle_error(turn, error),
        }
    }

    async fn execute_turn(&self, turn: &ActiveTurn) -> Result<TurnDecision, TurnError> {
        ensure_active(turn)?;
        let messages = fetch_messages(turn).await?;
        match plan_turn(&messages, turn.run_id(), turn.turn_number())? {
            ResumePlan::CallModel => self.call_model(turn, messages).await,
            ResumePlan::PrimaryModelCallInterrupted => Err(TurnError::PrimaryModelCallInterrupted),
            ResumePlan::ExecuteTools {
                assistant,
                missing_tool_calls,
                ..
            } => {
                let config = CodexHarnessConfig::from_resolved(&turn.request().resolved_config)?;
                let runtime = self.resolve_runtime(turn, &config).await?;
                self.run_tools(turn, &config, runtime, &assistant, &missing_tool_calls)
                    .await?;
                Ok(TurnDecision::Continue)
            }
            ResumePlan::Continue => Ok(TurnDecision::Continue),
            ResumePlan::Complete { final_message_id } => {
                Ok(TurnDecision::Complete(final_message_id))
            }
        }
    }

    async fn call_model(
        &self,
        turn: &ActiveTurn,
        mut session_messages: Vec<SessionMessage>,
    ) -> Result<TurnDecision, TurnError> {
        let config = CodexHarnessConfig::from_resolved(&turn.request().resolved_config)?;
        let runtime = self.resolve_runtime(turn, &config).await?;
        let environment = environment_snapshot(runtime.as_ref(), &config, &self.timezone)?;
        let mut request = form_main_request(
            &config,
            turn.request().session_id,
            &environment,
            &session_messages,
        )?;
        let prior_context_overflow = has_uncompacted_context_overflow(&session_messages);

        match plan_compaction(
            &request,
            &session_messages,
            config.model.profile().context_window,
            prior_context_overflow,
            turn.run_id(),
            turn.turn_number(),
        ) {
            CompactionPlan::Compact { trigger, .. } => {
                let compaction_request = form_compaction_request(
                    &config,
                    turn.request().session_id,
                    &environment,
                    &session_messages,
                )?;
                let response = self
                    .model
                    .complete(config.account_id, &compaction_request, turn.operation())
                    .await
                    .map_err(TurnError::CompactionModel)?;
                let checkpoint_id = Uuid::now_v7();
                let checkpoint = checkpoint_from_response(
                    &config,
                    MessageId::new(format!("codex-compaction-{checkpoint_id}"))
                        .expect("UUID-derived compaction message ID is valid"),
                    Timestamp(now_ms()),
                    trigger,
                    &request.messages,
                    &response,
                )?;
                let appended = append_messages(
                    turn,
                    &[NewRunMessage {
                        session_message_id: checkpoint_id,
                        message: Message::Custom(checkpoint),
                    }],
                )
                .await?;
                session_messages.extend(appended.items);
                request = form_main_request(
                    &config,
                    turn.request().session_id,
                    &environment,
                    &session_messages,
                )?;
            }
            CompactionPlan::NotRequired { .. } | CompactionPlan::AlreadyCompactedForTurn { .. } => {
            }
        }

        // This durable boundary is committed before sampling. If the process
        // dies after the provider accepts the request but before the assistant
        // append commits, broker redelivery fails explicitly instead of
        // sampling a second primary response for the same Agent turn.
        append_messages(turn, &[primary_call_started_message(turn)]).await?;
        let assistant = match self
            .model
            .complete(config.account_id, &request, turn.operation())
            .await
        {
            Ok(assistant) => assistant,
            Err(error) if error.kind == CodexModelFailureKind::ContextWindowExceeded => {
                append_messages(turn, &[context_overflow_message(turn)]).await?;
                return Ok(TurnDecision::Continue);
            }
            Err(error) => return Err(TurnError::PrimaryModel(error)),
        };
        ensure_active(turn)?;

        let assistant_session_message_id = Uuid::now_v7();
        append_messages(
            turn,
            &[NewRunMessage {
                session_message_id: assistant_session_message_id,
                message: Message::Assistant(assistant.clone()),
            }],
        )
        .await?;

        let tool_calls = assistant
            .content
            .iter()
            .filter(|content| matches!(content, AssistantContent::ToolCall { .. }))
            .cloned()
            .collect::<Vec<_>>();
        if tool_calls.is_empty() {
            return Ok(TurnDecision::Complete(assistant_session_message_id));
        }
        self.run_tools(turn, &config, runtime, &assistant, &tool_calls)
            .await?;
        Ok(TurnDecision::Continue)
    }

    async fn resolve_runtime(
        &self,
        turn: &ActiveTurn,
        config: &CodexHarnessConfig,
    ) -> Result<Arc<dyn ExecutionRuntime>, TurnError> {
        tokio::select! {
            () = turn.operation().cancelled() => Err(TurnError::Cancelled),
            runtime = self.execution.machine(&config.execution.machine_id) => {
                runtime.map_err(TurnError::Execution)
            }
        }
    }

    async fn run_tools(
        &self,
        turn: &ActiveTurn,
        config: &CodexHarnessConfig,
        runtime: Arc<dyn ExecutionRuntime>,
        assistant: &AssistantMessage,
        tool_calls: &[AssistantContent],
    ) -> Result<(), TurnError> {
        ensure_active(turn)?;
        let results = if assistant.stop_reason == StopReason::Length {
            tool_calls
                .iter()
                .map(truncated_tool_result)
                .collect::<Result<Vec<_>, _>>()?
        } else {
            let context = CodexToolExecutionContext {
                agent_session_id: turn.request().session_id,
                runtime,
                execution: config.execution.clone(),
                operation: turn.operation().clone(),
            };
            self.tools.execute_tool_calls(tool_calls, &context).await?
        };
        let messages = results
            .into_iter()
            .map(|result| NewRunMessage {
                session_message_id: Uuid::now_v7(),
                message: Message::ToolResult(result),
            })
            .collect::<Vec<_>>();
        append_messages(turn, &messages).await?;
        ensure_active(turn)
    }

    fn handle_error(
        &self,
        turn: &ActiveTurn,
        error: TurnError,
    ) -> Result<TurnOutcome, CodexRuntimeError> {
        if turn.operation().is_cancelled() || error.cancelled() {
            return Ok(TurnOutcome::Cancelled);
        }
        if error.agent_stale() {
            return Ok(TurnOutcome::Stale);
        }
        if error.agent_retryable() {
            let TurnError::Agent(source) = error else {
                unreachable!("agent_retryable only matches Agent")
            };
            return Err(CodexRuntimeError::Agent(source));
        }
        tracing::error!(
            run_id = %turn.run_id(),
            turn_number = turn.turn_number(),
            code = error.code(),
            error = ?error,
            "Codex turn failed"
        );
        Ok(TurnOutcome::Command(HarnessOperation::Fail {
            failure: JsonObject::from_iter([
                ("code".to_owned(), json!(error.code())),
                ("message".to_owned(), json!(error.to_string())),
            ]),
        }))
    }
}

#[async_trait::async_trait]
impl HarnessRuntime for CodexRuntime {
    type Error = CodexRuntimeError;

    async fn execute(&self, turn: &ActiveTurn) -> Result<TurnOutcome, Self::Error> {
        CodexRuntime::execute(self, turn).await
    }
}

#[derive(Debug, thiserror::Error)]
pub enum CodexRuntimeBuildError {
    #[error("could not construct the Codex model client")]
    Model(#[from] LlmGatewayClientError),
    #[error("could not construct the execution client")]
    Execution(#[from] execution_gateway_client::ExecutionGatewayClientError),
}

#[derive(Debug, thiserror::Error)]
pub enum CodexRuntimeError {
    #[error("Agent transcript access remained unavailable after retries")]
    Agent(#[source] ActiveTurnError),
}

#[derive(Debug, thiserror::Error)]
enum TurnError {
    #[error(transparent)]
    Config(#[from] CodexHarnessConfigError),
    #[error(transparent)]
    Transcript(#[from] TranscriptError),
    #[error(transparent)]
    Context(#[from] ContextFormationError),
    #[error(transparent)]
    Compaction(#[from] CompactionError),
    #[error("Codex compaction request failed: {0}")]
    CompactionModel(#[source] CodexModelCallError),
    #[error("Codex primary model request failed: {0}")]
    PrimaryModel(#[source] CodexModelCallError),
    #[error(transparent)]
    Agent(#[from] ActiveTurnError),
    #[error(transparent)]
    Execution(#[from] ExecutionResolutionError),
    #[error(transparent)]
    Environment(#[from] CodexEnvironmentError),
    #[error(transparent)]
    Tool(#[from] CodexToolDispatchError),
    #[error("expected an assistant tool call")]
    ExpectedToolCall,
    #[error(
        "the primary model call began but no assistant response was committed; refusing to sample a second response for this Agent turn"
    )]
    PrimaryModelCallInterrupted,
    #[error("the active run was cancelled")]
    Cancelled,
}

impl TurnError {
    fn code(&self) -> &'static str {
        match self {
            Self::Config(_) => "invalid_harness_config",
            Self::Transcript(_) => "invalid_turn_transcript",
            Self::Context(_) => "context_formation_failed",
            Self::Compaction(_) | Self::CompactionModel(_) => "compaction_failed",
            Self::PrimaryModel(_) => "llm_request_failed",
            Self::Agent(_) => "agent_request_failed",
            Self::Execution(_) | Self::Environment(_) => "execution_runtime_failed",
            Self::Tool(_) => "tool_execution_failed",
            Self::ExpectedToolCall => "invalid_tool_call",
            Self::PrimaryModelCallInterrupted => "primary_model_call_interrupted",
            Self::Cancelled => "cancelled",
        }
    }

    fn agent_retryable(&self) -> bool {
        matches!(self, Self::Agent(error) if error.retryable())
    }

    fn agent_stale(&self) -> bool {
        matches!(self, Self::Agent(error) if error.stale())
    }

    fn cancelled(&self) -> bool {
        match self {
            Self::Cancelled => true,
            Self::PrimaryModel(error) | Self::CompactionModel(error) => {
                error.kind == CodexModelFailureKind::Cancelled
            }
            _ => false,
        }
    }
}

enum TurnDecision {
    Complete(Uuid),
    Continue,
}

impl TurnDecision {
    fn finish(self) -> TurnOutcome {
        match self {
            Self::Complete(final_message_id) => {
                TurnOutcome::Command(HarnessOperation::Complete { final_message_id })
            }
            Self::Continue => TurnOutcome::Command(HarnessOperation::Continue),
        }
    }
}

fn environment_snapshot(
    runtime: &dyn ExecutionRuntime,
    config: &CodexHarnessConfig,
    timezone: &str,
) -> Result<CodexEnvironmentSnapshot, CodexEnvironmentError> {
    CodexEnvironmentSnapshot::from_execution_runtime(
        runtime,
        &config.execution,
        Local::now().format("%Y-%m-%d").to_string(),
        timezone,
        Timestamp(now_ms()),
    )
}

fn has_uncompacted_context_overflow(messages: &[SessionMessage]) -> bool {
    let latest_overflow = latest_tag_revision(messages, CODEX_CONTEXT_OVERFLOW_TAG);
    let latest_compaction = latest_tag_revision(messages, CODEX_COMPACTION_MESSAGE_TAG);
    latest_overflow
        .is_some_and(|overflow| latest_compaction.is_none_or(|compact| overflow > compact))
}

fn latest_tag_revision(messages: &[SessionMessage], tag: &str) -> Option<u64> {
    messages
        .iter()
        .filter_map(|message| match &message.message {
            Message::Custom(custom) if custom.tag.as_deref() == Some(tag) => Some(message.revision),
            _ => None,
        })
        .max()
}

fn primary_call_started_message(turn: &ActiveTurn) -> NewRunMessage {
    marker_message(
        CODEX_PRIMARY_CALL_STARTED_TAG,
        json!({
            "event_id": turn.request().event_id,
            "run_id": turn.run_id(),
            "turn_number": turn.turn_number()
        }),
    )
}

fn context_overflow_message(turn: &ActiveTurn) -> NewRunMessage {
    marker_message(
        CODEX_CONTEXT_OVERFLOW_TAG,
        json!({
            "run_id": turn.run_id(),
            "turn_number": turn.turn_number()
        }),
    )
}

fn marker_message(tag: &str, content: serde_json::Value) -> NewRunMessage {
    let session_message_id = Uuid::now_v7();
    NewRunMessage {
        session_message_id,
        message: Message::Custom(CustomMessage {
            id: MessageId::new(format!("{tag}-{session_message_id}"))
                .expect("tag and UUID form a valid message ID"),
            content: content
                .as_object()
                .expect("marker content is an object")
                .clone(),
            tag: Some(tag.to_owned()),
            timestamp: Timestamp(now_ms()),
        }),
    }
}

fn truncated_tool_result(content: &AssistantContent) -> Result<ToolResultMessage, TurnError> {
    let AssistantContent::ToolCall {
        name, tool_call_id, ..
    } = content
    else {
        return Err(TurnError::ExpectedToolCall);
    };
    let message = format!(
        "Tool call `{name}` was not executed because the model response hit its output token limit and its arguments may be incomplete. Re-issue the tool call with complete arguments."
    );
    Ok(ToolResultMessage {
        id: MessageId::new(format!("tool-result-{}", Uuid::now_v7()))
            .expect("UUID-derived tool result ID is valid"),
        tool_name: name.clone(),
        tool_call_id: tool_call_id.clone(),
        content: vec![ContentPart::Text(TextContent {
            content: message.clone(),
            metadata: None,
        })],
        details: None,
        timestamp: Timestamp(now_ms()),
        outcome: ToolResultOutcome::Error {
            error: ToolResultError {
                message,
                name: Some("truncated_tool_call".to_owned()),
            },
        },
    })
}

async fn fetch_messages(turn: &ActiveTurn) -> Result<Vec<SessionMessage>, TurnError> {
    let mut retries = 0;
    loop {
        let result = tokio::select! {
            () = turn.operation().cancelled() => return Err(TurnError::Cancelled),
            result = turn.fetch_session_messages() => result,
        };
        match result {
            Ok(messages) => return Ok(messages),
            Err(error) if retries < AGENT_MAX_RETRIES && error.retryable() => {
                retries += 1;
                agent_retry_sleep(turn.operation(), retries).await?;
            }
            Err(error) => return Err(error.into()),
        }
    }
}

async fn append_messages(
    turn: &ActiveTurn,
    messages: &[NewRunMessage],
) -> Result<SessionMessagesAppended, TurnError> {
    let mut retries = 0;
    loop {
        let result = tokio::select! {
            () = turn.operation().cancelled() => return Err(TurnError::Cancelled),
            result = turn.append_messages(messages) => result,
        };
        match result {
            Ok(appended) => return Ok(appended),
            Err(error) if retries < AGENT_MAX_RETRIES && error.retryable() => {
                retries += 1;
                agent_retry_sleep(turn.operation(), retries).await?;
            }
            Err(error) => return Err(error.into()),
        }
    }
}

async fn agent_retry_sleep(operation: &OperationContext, retry: u32) -> Result<(), TurnError> {
    let multiplier = 1_u32
        .checked_shl(retry.saturating_sub(1))
        .unwrap_or(u32::MAX);
    let delay = AGENT_RETRY_BASE.saturating_mul(multiplier);
    tokio::select! {
        () = operation.cancelled() => Err(TurnError::Cancelled),
        () = tokio::time::sleep(delay) => Ok(()),
    }
}

fn ensure_active(turn: &ActiveTurn) -> Result<(), TurnError> {
    if turn.operation().is_cancelled() {
        Err(TurnError::Cancelled)
    } else {
        Ok(())
    }
}

fn now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .ok()
        .and_then(|duration| u64::try_from(duration.as_millis()).ok())
        .unwrap_or(0)
}
