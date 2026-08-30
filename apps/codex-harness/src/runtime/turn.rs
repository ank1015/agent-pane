use std::{sync::Arc, time::Duration};

use agent_contracts::{
    HarnessOperation, NewRunMessage, SessionMessage, SessionMessageOrigin, SessionMessagesAppended,
};
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

use super::environment::{
    CodexEnvironmentPlacement, is_environment_message, latest_environment_snapshot,
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
        let split = split_pending_user_input(&session_messages);
        // Match Codex's pre-turn boundary: assess and, if needed, compact only
        // history that existed before the newly delivered user input. Agent has
        // already committed that input, so a durable checkpoint carries it as a
        // pending suffix without exposing it to the compaction model.
        let pre_turn_request =
            form_main_request(&config, turn.request().session_id, &split.compactable)?;
        let prior_context_overflow = has_uncompacted_context_overflow(&session_messages);

        match plan_compaction(
            &pre_turn_request,
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
                    &split.compactable,
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
                    &pre_turn_request.messages,
                    &split.pending_messages,
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
                let placement = environment_placement_after_compaction(
                    &split.pending_messages,
                    &pre_turn_request.messages,
                );
                append_environment_update(
                    turn,
                    &mut session_messages,
                    &environment,
                    placement,
                    /*force_full*/ true,
                )
                .await?;
            }
            CompactionPlan::NotRequired { .. } => {
                let placement = environment_placement_for_sampling(
                    &split.pending_messages,
                    &pre_turn_request.messages,
                );
                append_environment_update(
                    turn,
                    &mut session_messages,
                    &environment,
                    placement,
                    /*force_full*/ false,
                )
                .await?;
            }
            CompactionPlan::AlreadyCompactedForTurn { .. } => {
                // A checkpoint and its refreshed environment are two separate
                // Agent appends. If the process died between them, redelivery
                // must restore the full context without compacting again.
                let needs_reinjection = !environment_survives_latest_compaction(&session_messages);
                let placement = if needs_reinjection {
                    environment_placement_for_existing_compaction(&pre_turn_request.messages)
                } else {
                    environment_placement_for_sampling(
                        &split.pending_messages,
                        &pre_turn_request.messages,
                    )
                };
                append_environment_update(
                    turn,
                    &mut session_messages,
                    &environment,
                    placement,
                    /*force_full*/ false,
                )
                .await?;
            }
        }

        let request = form_main_request(&config, turn.request().session_id, &session_messages)?;

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
        append_tool_results(turn, results).await?;
        if turn.operation().is_cancelled() {
            Err(TurnError::Cancelled)
        } else {
            Ok(())
        }
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

#[derive(Debug)]
struct PendingUserInputSplit {
    compactable: Vec<SessionMessage>,
    pending_messages: Vec<Message>,
}

fn split_pending_user_input(messages: &[SessionMessage]) -> PendingUserInputSplit {
    let mut ordered = messages.to_vec();
    ordered.sort_by_key(|message| message.revision);
    let latest_sampled_revision = ordered
        .iter()
        .filter(|message| {
            matches!(message.message, Message::Assistant(_))
                || matches!(
                    &message.message,
                    Message::Custom(custom)
                        if custom.tag.as_deref() == Some(CODEX_COMPACTION_MESSAGE_TAG)
                )
        })
        .map(|message| message.revision)
        .max()
        .unwrap_or(0);
    let pending_start = ordered.iter().position(|message| {
        message.revision > latest_sampled_revision
            && matches!(
                message.origin,
                SessionMessageOrigin::External | SessionMessageOrigin::Imported
            )
            && matches!(message.message, Message::User(_))
            && !is_environment_message(&message.message)
    });
    let Some(pending_start) = pending_start else {
        return PendingUserInputSplit {
            compactable: ordered,
            pending_messages: Vec::new(),
        };
    };

    let pending_messages = ordered[pending_start..]
        .iter()
        .filter_map(|message| match &message.message {
            // A previously captured environment belongs to the pending context
            // boundary but is reinjected from the current immutable snapshot.
            message if is_environment_message(message) => None,
            Message::Custom(_) => None,
            message => Some(message.clone()),
        })
        .collect();
    ordered.truncate(pending_start);
    PendingUserInputSplit {
        compactable: ordered,
        pending_messages,
    }
}

fn environment_placement_for_sampling(
    pending_messages: &[Message],
    compactable_messages: &[Message],
) -> CodexEnvironmentPlacement {
    if let Some(message_id) = pending_messages.iter().find_map(real_user_message_id) {
        return CodexEnvironmentPlacement::BeforeMessage { message_id };
    }
    compactable_messages
        .iter()
        .rev()
        .find(|message| !is_environment_message(message))
        .map_or(CodexEnvironmentPlacement::End, |message| {
            let message_id = message_id_of(message).clone();
            if matches!(message, Message::User(_)) {
                CodexEnvironmentPlacement::BeforeMessage { message_id }
            } else {
                CodexEnvironmentPlacement::AfterMessage { message_id }
            }
        })
}

fn environment_placement_after_compaction(
    pending_messages: &[Message],
    compacted_messages: &[Message],
) -> CodexEnvironmentPlacement {
    pending_messages
        .iter()
        .find_map(real_user_message_id)
        .or_else(|| {
            compacted_messages
                .iter()
                .rev()
                .find_map(real_user_message_id)
        })
        .map_or(CodexEnvironmentPlacement::BeforeCompaction, |message_id| {
            CodexEnvironmentPlacement::BeforeMessage { message_id }
        })
}

fn environment_placement_for_existing_compaction(
    messages: &[Message],
) -> CodexEnvironmentPlacement {
    let compaction_index = messages.iter().rposition(|message| {
        let Message::Custom(message) = message else {
            return false;
        };
        message
            .content
            .get("items")
            .and_then(serde_json::Value::as_array)
            .is_some_and(|items| {
                items.iter().any(|item| {
                    matches!(
                        item.get("type").and_then(serde_json::Value::as_str),
                        Some("compaction" | "compaction_summary")
                    )
                })
            })
    });
    if let Some(message_id) = compaction_index
        .and_then(|index| messages[index + 1..].iter().find_map(real_user_message_id))
    {
        CodexEnvironmentPlacement::BeforeMessage { message_id }
    } else {
        environment_placement_after_compaction(&[], messages)
    }
}

async fn append_environment_update(
    turn: &ActiveTurn,
    session_messages: &mut Vec<SessionMessage>,
    environment: &CodexEnvironmentSnapshot,
    placement: CodexEnvironmentPlacement,
    force_full: bool,
) -> Result<(), TurnError> {
    let previous = latest_environment_snapshot(session_messages);
    let force_full = force_full || !environment_survives_latest_compaction(session_messages);
    let session_message_id = Uuid::now_v7();
    let Some(message) = environment.as_persisted_message(
        MessageId::new(format!("codex-environment-{session_message_id}"))
            .expect("UUID-derived environment message ID is valid"),
        placement,
        previous.as_ref(),
        force_full,
    ) else {
        return Ok(());
    };
    let appended = append_messages(
        turn,
        &[NewRunMessage {
            session_message_id,
            message,
        }],
    )
    .await?;
    session_messages.extend(appended.items);
    Ok(())
}

fn environment_survives_latest_compaction(messages: &[SessionMessage]) -> bool {
    let Some(compaction_revision) = latest_tag_revision(messages, CODEX_COMPACTION_MESSAGE_TAG)
    else {
        return true;
    };
    messages.iter().any(|message| {
        message.revision > compaction_revision && is_environment_message(&message.message)
    })
}

fn real_user_message_id(message: &Message) -> Option<MessageId> {
    let Message::User(message) = message else {
        return None;
    };
    (!message.id.as_str().starts_with("codex-environment-")).then(|| message.id.clone())
}

fn message_id_of(message: &Message) -> &MessageId {
    match message {
        Message::User(message) => &message.id,
        Message::System(message) => &message.id,
        Message::ToolResult(message) => &message.id,
        Message::Assistant(message) => &message.id,
        Message::Custom(message) => &message.id,
    }
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

async fn append_tool_results(
    turn: &ActiveTurn,
    results: Vec<ToolResultMessage>,
) -> Result<(), TurnError> {
    let messages = results
        .into_iter()
        .map(|result| NewRunMessage {
            session_message_id: Uuid::now_v7(),
            message: Message::ToolResult(result),
        })
        .collect::<Vec<_>>();
    // Cancellation is itself a result-producing terminal condition in Codex.
    // Once calls are committed, pair them before this broker turn exits even
    // though the turn-scoped operation has already been cancelled.
    append_messages_ignoring_cancellation(turn, &messages).await?;
    Ok(())
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

async fn append_messages_ignoring_cancellation(
    turn: &ActiveTurn,
    messages: &[NewRunMessage],
) -> Result<SessionMessagesAppended, TurnError> {
    let mut retries = 0;
    loop {
        match turn.append_messages(messages).await {
            Ok(appended) => return Ok(appended),
            Err(error) if matches!(error.code(), Some("run_state_conflict" | "run_not_active")) => {
                break;
            }
            Err(error) if retries < AGENT_MAX_RETRIES && error.retryable() => {
                retries += 1;
                let multiplier = 1_u32
                    .checked_shl(retries.saturating_sub(1))
                    .unwrap_or(u32::MAX);
                tokio::time::sleep(AGENT_RETRY_BASE.saturating_mul(multiplier)).await;
            }
            Err(error) => return Err(error.into()),
        }
    }

    // A durable Agent abort advances the run fence before publishing the
    // cancellation event. Retry the exact same IDs through the server's
    // tool-result-only cleanup path. If cancellation instead came from local
    // harness shutdown, the ordinary append above succeeds while the run is
    // still Active and this exception is never requested.
    retries = 0;
    loop {
        match turn.append_cancelled_tool_results(messages).await {
            Ok(appended) => return Ok(appended),
            Err(error) if retries < AGENT_MAX_RETRIES && error.retryable() => {
                retries += 1;
                let multiplier = 1_u32
                    .checked_shl(retries.saturating_sub(1))
                    .unwrap_or(u32::MAX);
                tokio::time::sleep(AGENT_RETRY_BASE.saturating_mul(multiplier)).await;
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

#[cfg(test)]
mod tests {
    use agent_contracts::{SessionMessageDelivery, SessionMessageOrigin};
    use chrono::Utc;
    use llm_contracts::{
        AssistantMessage, ContentPart, CustomMessage, Message, MessageId, ModelId, ModelRef,
        ProviderId, StopReason, TextContent, Timestamp, UserMessage,
    };
    use serde_json::json;
    use uuid::Uuid;

    use super::{
        CODEX_COMPACTION_MESSAGE_TAG, CodexEnvironmentPlacement, CodexEnvironmentSnapshot,
        environment_placement_for_existing_compaction, environment_placement_for_sampling,
        environment_survives_latest_compaction, split_pending_user_input,
    };

    #[test]
    fn excludes_new_external_user_messages_from_the_compaction_snapshot() {
        let session_id = Uuid::now_v7();
        let messages = vec![
            session_message(
                session_id,
                1,
                SessionMessageOrigin::External,
                user("user-1", "first turn"),
            ),
            session_message(
                session_id,
                2,
                SessionMessageOrigin::Harness,
                assistant("assistant-1"),
            ),
            session_message(
                session_id,
                3,
                SessionMessageOrigin::External,
                user("user-2", "current turn"),
            ),
            session_message(
                session_id,
                4,
                SessionMessageOrigin::Imported,
                user("user-3", "steered before sampling"),
            ),
        ];

        let split = split_pending_user_input(&messages);
        assert_eq!(split.compactable.len(), 2);
        assert_eq!(message_id(&split.compactable[0].message), "user-1");
        assert_eq!(message_id(&split.compactable[1].message), "assistant-1");
        assert_eq!(split.pending_messages.len(), 2);
        assert_eq!(message_id(&split.pending_messages[0]), "user-2");
        assert_eq!(message_id(&split.pending_messages[1]), "user-3");
        assert_eq!(
            environment_placement_for_sampling(
                &split.pending_messages,
                &split
                    .compactable
                    .iter()
                    .map(|message| message.message.clone())
                    .collect::<Vec<_>>(),
            ),
            CodexEnvironmentPlacement::BeforeMessage {
                message_id: MessageId::new("user-2").expect("message ID")
            }
        );
    }

    #[test]
    fn keeps_tool_follow_up_history_compactable_when_no_user_is_pending() {
        let session_id = Uuid::now_v7();
        let messages = vec![
            session_message(
                session_id,
                1,
                SessionMessageOrigin::External,
                user("user-1", "run a tool"),
            ),
            session_message(
                session_id,
                2,
                SessionMessageOrigin::Harness,
                assistant("assistant-1"),
            ),
        ];

        let split = split_pending_user_input(&messages);
        assert_eq!(split.compactable.len(), 2);
        assert!(split.pending_messages.is_empty());
    }

    #[test]
    fn detects_a_checkpoint_committed_before_its_environment_reinjection() {
        let session_id = Uuid::now_v7();
        let old_environment = environment_message("old-environment", Timestamp(1));
        let checkpoint = Message::Custom(CustomMessage {
            id: MessageId::new("checkpoint").expect("message ID"),
            content: serde_json::Map::new(),
            tag: Some(CODEX_COMPACTION_MESSAGE_TAG.to_owned()),
            timestamp: Timestamp(2),
        });
        let mut messages = vec![
            session_message(
                session_id,
                1,
                SessionMessageOrigin::Harness,
                old_environment,
            ),
            session_message(session_id, 2, SessionMessageOrigin::Harness, checkpoint),
        ];
        assert!(!environment_survives_latest_compaction(&messages));

        messages.push(session_message(
            session_id,
            3,
            SessionMessageOrigin::Harness,
            environment_message("new-environment", Timestamp(3)),
        ));
        assert!(environment_survives_latest_compaction(&messages));
    }

    #[test]
    fn crash_recovery_reinjects_before_the_user_already_carried_by_the_checkpoint() {
        let native_compaction = Message::Custom(CustomMessage {
            id: MessageId::new("checkpoint-native").expect("message ID"),
            content: json!({
                "items": [{"type": "compaction", "encrypted_content": "opaque"}]
            })
            .as_object()
            .expect("object")
            .clone(),
            tag: Some(provider_openai::OPENAI_NATIVE_INPUT_TAG.to_owned()),
            timestamp: Timestamp(2),
        });
        let messages = vec![
            user("retained-user", "retained"),
            native_compaction,
            user("checkpoint-pending-user", "original current input"),
            user("newly-steered-user", "arrived during recovery"),
        ];

        assert_eq!(
            environment_placement_for_existing_compaction(&messages),
            CodexEnvironmentPlacement::BeforeMessage {
                message_id: MessageId::new("checkpoint-pending-user").expect("message ID")
            }
        );
    }

    fn session_message(
        session_id: Uuid,
        revision: u64,
        origin: SessionMessageOrigin,
        message: Message,
    ) -> agent_contracts::SessionMessage {
        agent_contracts::SessionMessage {
            session_message_id: Uuid::now_v7(),
            session_id,
            revision,
            message,
            origin,
            delivery: SessionMessageDelivery::Immediate,
            run_id: None,
            turn_number: None,
            created_at: Utc::now(),
            committed_at: Utc::now(),
        }
    }

    fn user(id: &str, content: &str) -> Message {
        Message::User(UserMessage {
            id: MessageId::new(id).expect("message ID"),
            timestamp: Timestamp(1),
            content: vec![ContentPart::Text(TextContent {
                content: content.to_owned(),
                metadata: None,
            })],
        })
    }

    fn assistant(id: &str) -> Message {
        Message::Assistant(AssistantMessage {
            id: MessageId::new(id).expect("message ID"),
            model: ModelRef {
                provider: ProviderId::new("openai").expect("provider ID"),
                id: ModelId::new("gpt-5.6-sol").expect("model ID"),
                name: None,
            },
            usage: None,
            duration_ms: 1,
            native_message: json!({"output": []}),
            content: Vec::new(),
            stop_reason: StopReason::Stop,
            timestamp: Timestamp(2),
        })
    }

    fn environment_message(id: &str, timestamp: Timestamp) -> Message {
        CodexEnvironmentSnapshot::new(
            "/workspace",
            Some("zsh".to_owned()),
            "2026-08-30",
            "UTC",
            vec!["/workspace".to_owned()],
            timestamp,
        )
        .expect("environment")
        .as_persisted_message(
            MessageId::new(format!("codex-environment-{id}")).expect("message ID"),
            CodexEnvironmentPlacement::End,
            None,
            true,
        )
        .expect("full environment")
    }

    fn message_id(message: &Message) -> &str {
        match message {
            Message::User(message) => message.id.as_str(),
            Message::System(message) => message.id.as_str(),
            Message::ToolResult(message) => message.id.as_str(),
            Message::Assistant(message) => message.id.as_str(),
            Message::Custom(message) => message.id.as_str(),
        }
    }
}
