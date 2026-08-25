use std::{collections::BTreeMap, time::Duration};

use agent_contracts::{
    NewRunMessage, RunAbortAcknowledged, RunMessagesAppended, RunTurnCompleted, RunTurnFailed,
    SessionMessage,
};
use execution_runtime::{ExecutionRuntime, OperationContext};
use futures_util::future::join_all;
use llm_contracts::{
    AssistantContent, AssistantMessage, ContentPart, JsonObject, LlmRequest, Message, MessageId,
    StopReason, TextContent, Timestamp, ToolResultError, ToolResultMessage, ToolResultOutcome,
};
use serde_json::json;
use uuid::Uuid;

use crate::{
    clients::{ExecutionClient, LlmGatewayClient},
    config::WorkerConfig,
    harness::{
        compact::{
            PiCompactionMessageContent, create_pi_compaction_message, form_compaction_request,
            select_first_kept_message_id,
        },
        context_formation::{ContextFormationError, form_context_messages},
        model_catalog::MODEL_CATALOG,
        model_resolver::get_model_config,
        system_prompt::generate_system_prompt,
        tools::{ToolExecutionContext, WorkspaceCwd, default_tool_definitions, execute_tool_call},
    },
    worker::{ActiveRun, RunInterruption},
};

use super::{
    config::PiHarnessConfig,
    error::{PiRuntimeBuildError, PiRuntimeError, TurnError},
    retry::{RetryPolicy, complete_with_retry},
    transcript::{ResumePlan, plan_turn},
};

const AGENT_MAX_RETRIES: u32 = 3;
const AGENT_RETRY_BASE: Duration = Duration::from_millis(250);

#[derive(Clone)]
pub struct PiRuntime {
    llm: LlmGatewayClient,
    execution: ExecutionClient,
    retry: RetryPolicy,
}

impl PiRuntime {
    pub fn from_config(config: &WorkerConfig) -> Result<Self, PiRuntimeBuildError> {
        Ok(Self::new(
            LlmGatewayClient::new(config.llm_gateway.clone())?,
            ExecutionClient::new(config.execution_gateway.clone())?,
        ))
    }

    #[must_use]
    pub fn new(llm: LlmGatewayClient, execution: ExecutionClient) -> Self {
        Self {
            llm,
            execution,
            retry: RetryPolicy::pi_default(),
        }
    }

    #[must_use]
    pub fn with_retry_policy(mut self, retry: RetryPolicy) -> Self {
        self.retry = retry;
        self
    }

    pub async fn execute(&self, run: ActiveRun) -> Result<RunOutcome, PiRuntimeError> {
        let result = self.execute_turn(&run).await;
        match result {
            Ok(decision) => self.finish(run, decision).await,
            Err(error) => self.handle_error(run, error).await,
        }
    }

    async fn execute_turn(&self, run: &ActiveRun) -> Result<TurnDecision, TurnError> {
        ensure_active(run)?;
        let messages = fetch_messages(run).await?;
        let turn = run.claimed().run.current_turn;
        match plan_turn(&messages, run.run_id(), turn)? {
            ResumePlan::CallModel => {
                let config = PiHarnessConfig::from_resolved(&run.claimed().run.resolved_config)?;
                self.call_model(run, &config, messages).await
            }
            ResumePlan::Complete { final_message_id } => {
                Ok(TurnDecision::Complete(final_message_id))
            }
            ResumePlan::ExecuteTools {
                assistant,
                missing_tool_calls,
            } => {
                let config = PiHarnessConfig::from_resolved(&run.claimed().run.resolved_config)?;
                self.run_tools(run, &config, &assistant, &missing_tool_calls)
                    .await?;
                Ok(TurnDecision::Continue)
            }
            ResumePlan::Continue => Ok(TurnDecision::Continue),
        }
    }

    async fn call_model(
        &self,
        run: &ActiveRun,
        config: &PiHarnessConfig,
        session_messages: Vec<SessionMessage>,
    ) -> Result<TurnDecision, TurnError> {
        let mut messages = session_messages
            .into_iter()
            .map(|message| message.message)
            .collect::<Vec<_>>();
        let mut compacted = false;

        loop {
            let context = match form_context_messages(&messages, &config.provider, &config.model_id)
            {
                Ok(context) => context,
                Err(ContextFormationError::CompactionRequired { .. }) if !compacted => {
                    self.compact(run, config, &mut messages).await?;
                    compacted = true;
                    continue;
                }
                Err(error) => return Err(error.into()),
            };
            let request = main_request(run, config, context)?;
            match complete_with_retry(
                &self.llm,
                config.account_id,
                &request,
                run.operation(),
                self.retry,
            )
            .await
            {
                Ok(assistant) => {
                    let session_message_id = Uuid::now_v7();
                    append_messages(
                        run,
                        &[NewRunMessage {
                            session_message_id,
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
                        return Ok(TurnDecision::Complete(session_message_id));
                    }
                    self.run_tools(run, config, &assistant, &tool_calls).await?;
                    return Ok(TurnDecision::Continue);
                }
                Err(error) if error.is_context_overflow() && !compacted => {
                    self.compact(run, config, &mut messages).await?;
                    compacted = true;
                }
                Err(error) => return Err(error.into()),
            }
        }
    }

    async fn compact(
        &self,
        run: &ActiveRun,
        config: &PiHarnessConfig,
        messages: &mut Vec<Message>,
    ) -> Result<(), TurnError> {
        let first_kept_message_id =
            select_first_kept_message_id(messages).ok_or(TurnError::NoCompactionCutPoint)?;
        let request = form_compaction_request(
            messages,
            &config.provider,
            &config.model_id,
            &config.reasoning_level,
        )?;
        let summary = complete_with_retry(
            &self.llm,
            config.account_id,
            &request,
            run.operation(),
            self.retry,
        )
        .await?;
        let text = assistant_text(&summary);
        if text.is_empty() {
            return Err(TurnError::EmptyCompactionSummary);
        }
        let session_message_id = Uuid::now_v7();
        let message = Message::Custom(create_pi_compaction_message(
            MessageId::new(format!("pi-compaction-{session_message_id}"))
                .expect("UUID compaction message ID is valid"),
            Timestamp(now_ms()),
            PiCompactionMessageContent {
                summary: text,
                first_kept_message_id,
                usage: summary.usage,
            },
        )?);
        append_messages(
            run,
            &[NewRunMessage {
                session_message_id,
                message: message.clone(),
            }],
        )
        .await?;
        messages.push(message);
        Ok(())
    }

    async fn run_tools(
        &self,
        run: &ActiveRun,
        config: &PiHarnessConfig,
        assistant: &AssistantMessage,
        tool_calls: &[AssistantContent],
    ) -> Result<(), TurnError> {
        if run.operation().is_cancelled() {
            return close_aborted_tool_calls(run, tool_calls).await;
        }
        let results = if assistant.stop_reason == StopReason::Length {
            tool_calls
                .iter()
                .map(truncated_tool_result)
                .collect::<Result<Vec<_>, _>>()?
        } else {
            let machine = tokio::select! {
                () = run.operation().cancelled() => {
                    return close_aborted_tool_calls(run, tool_calls).await;
                }
                machine = self.execution.machine(&config.execution.machine_id) => machine?,
            };
            if !machine
                .descriptor()
                .workspace_roots
                .iter()
                .any(|root| root.id == config.execution.workspace_root_id)
            {
                return Err(TurnError::WorkspaceRootNotFound(
                    config.execution.workspace_root_id.to_string(),
                ));
            }
            let cwd = WorkspaceCwd::new(
                config.execution.workspace_root_id.clone(),
                &config.execution.cwd,
            )?;
            let context = ToolExecutionContext {
                runtime: &machine,
                cwd: &cwd,
                operation: run.operation(),
            };
            join_all(
                tool_calls
                    .iter()
                    .map(|tool_call| execute_tool_call(tool_call, &context)),
            )
            .await
            .into_iter()
            .collect::<Result<Vec<_>, _>>()?
        };
        let messages = results
            .into_iter()
            .map(|result| NewRunMessage {
                session_message_id: Uuid::now_v7(),
                message: Message::ToolResult(result),
            })
            .collect::<Vec<_>>();
        append_messages(run, &messages).await?;
        ensure_active(run)?;
        Ok(())
    }

    async fn finish(
        &self,
        run: ActiveRun,
        decision: TurnDecision,
    ) -> Result<RunOutcome, PiRuntimeError> {
        if let Some(interruption) = run.interruption() {
            return finish_interruption(run, interruption).await;
        }
        match decision {
            TurnDecision::Complete(final_message_id) => run
                .complete(final_message_id)
                .await
                .map(RunOutcome::Completed)
                .map_err(PiRuntimeError::Agent),
            TurnDecision::Continue => run
                .continue_turn()
                .await
                .map(RunOutcome::Continued)
                .map_err(PiRuntimeError::Agent),
        }
    }

    async fn handle_error(
        &self,
        run: ActiveRun,
        error: TurnError,
    ) -> Result<RunOutcome, PiRuntimeError> {
        if let Some(interruption) = run.interruption() {
            return finish_interruption(run, interruption).await;
        }
        if run.operation().is_cancelled() {
            return Err(PiRuntimeError::Cancelled);
        }
        if error.lease_is_lost() {
            return Err(PiRuntimeError::LeaseLost);
        }
        let failure = JsonObject::from_iter([
            ("code".to_owned(), json!(error.code())),
            ("message".to_owned(), json!(error.to_string())),
        ]);
        run.fail(failure)
            .await
            .map(RunOutcome::Failed)
            .map_err(PiRuntimeError::Agent)
    }
}

fn main_request(
    run: &ActiveRun,
    config: &PiHarnessConfig,
    messages: Vec<Message>,
) -> Result<LlmRequest, TurnError> {
    let model = get_model_config(
        &config.provider,
        &config.model_id,
        &config.reasoning_level,
        &run.claimed().run.session_id.to_string(),
    )?;
    let mut provider_options = model.provider_options;
    let max_tokens = MODEL_CATALOG
        .iter()
        .find(|entry| entry.provider == config.provider && entry.id == config.model_id)
        .expect("model resolver already verified the catalog entry")
        .max_tokens;
    provider_options.insert("max_output_tokens".to_owned(), json!(max_tokens));
    Ok(LlmRequest {
        model: model.model,
        instructions: Some(generate_system_prompt(
            config.external_prompt.as_deref(),
            config.is_replaced,
        )),
        messages,
        tools: default_tool_definitions(),
        provider_options,
        metadata: BTreeMap::new(),
    })
}

async fn fetch_messages(run: &ActiveRun) -> Result<Vec<SessionMessage>, TurnError> {
    let mut retries = 0;
    loop {
        let result = tokio::select! {
            () = run.operation().cancelled() => return Err(TurnError::Cancelled),
            result = run.fetch_session_messages() => result,
        };
        match result {
            Ok(messages) => return Ok(messages),
            Err(error) if retries < AGENT_MAX_RETRIES && error.retryable() => {
                retries += 1;
                agent_retry_sleep(run.operation(), retries).await?;
            }
            Err(error) => return Err(error.into()),
        }
    }
}

async fn append_messages(
    run: &ActiveRun,
    messages: &[NewRunMessage],
) -> Result<RunMessagesAppended, TurnError> {
    let mut retries = 0;
    loop {
        let result = run.append_messages(messages).await;
        match result {
            Ok(appended) => return Ok(appended),
            Err(error) if retries < AGENT_MAX_RETRIES && error.retryable() => {
                retries += 1;
                tokio::time::sleep(agent_retry_delay(retries)).await;
            }
            Err(error) => return Err(error.into()),
        }
    }
}

async fn agent_retry_sleep(operation: &OperationContext, retry: u32) -> Result<(), TurnError> {
    let delay = agent_retry_delay(retry);
    tokio::select! {
        () = operation.cancelled() => Err(TurnError::Cancelled),
        () = tokio::time::sleep(delay) => Ok(()),
    }
}

fn agent_retry_delay(retry: u32) -> Duration {
    let multiplier = 1_u32
        .checked_shl(retry.saturating_sub(1))
        .unwrap_or(u32::MAX);
    AGENT_RETRY_BASE.saturating_mul(multiplier)
}

async fn close_aborted_tool_calls(
    run: &ActiveRun,
    tool_calls: &[AssistantContent],
) -> Result<(), TurnError> {
    if !matches!(run.interruption(), Some(RunInterruption::Abort(_))) {
        return Err(TurnError::Cancelled);
    }
    let messages = tool_calls
        .iter()
        .map(aborted_tool_result)
        .collect::<Result<Vec<_>, _>>()?
        .into_iter()
        .map(|result| NewRunMessage {
            session_message_id: Uuid::now_v7(),
            message: Message::ToolResult(result),
        })
        .collect::<Vec<_>>();
    append_messages(run, &messages).await?;
    Err(TurnError::Cancelled)
}

fn aborted_tool_result(content: &AssistantContent) -> Result<ToolResultMessage, TurnError> {
    let AssistantContent::ToolCall {
        name, tool_call_id, ..
    } = content
    else {
        return Err(TurnError::ExpectedToolCall);
    };
    let message = if name == "bash" {
        "Command aborted"
    } else {
        "Operation aborted"
    };
    Ok(ToolResultMessage {
        id: MessageId::new(format!("tool-result-{}", Uuid::now_v7()))
            .expect("UUID tool result ID is valid"),
        tool_name: name.clone(),
        tool_call_id: tool_call_id.clone(),
        content: vec![ContentPart::Text(TextContent {
            content: message.to_owned(),
            metadata: None,
        })],
        details: None,
        timestamp: Timestamp(now_ms()),
        outcome: ToolResultOutcome::Error {
            error: ToolResultError {
                message: message.to_owned(),
                name: Some("cancelled".to_owned()),
            },
        },
    })
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
            .expect("UUID tool result ID is valid"),
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

fn assistant_text(message: &AssistantMessage) -> String {
    message
        .content
        .iter()
        .filter_map(|content| match content {
            AssistantContent::Response { response } => Some(response.content.as_str()),
            _ => None,
        })
        .collect::<Vec<_>>()
        .join("\n")
        .trim()
        .to_owned()
}

fn ensure_active(run: &ActiveRun) -> Result<(), TurnError> {
    if run.operation().is_cancelled() {
        Err(TurnError::Cancelled)
    } else {
        Ok(())
    }
}

async fn finish_interruption(
    run: ActiveRun,
    interruption: RunInterruption,
) -> Result<RunOutcome, PiRuntimeError> {
    match interruption {
        RunInterruption::Abort(_) => run
            .acknowledge_abort(JsonObject::new())
            .await
            .map(RunOutcome::Aborted)
            .map_err(PiRuntimeError::Agent),
        RunInterruption::LeaseLost => Err(PiRuntimeError::LeaseLost),
    }
}

fn now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .ok()
        .and_then(|duration| u64::try_from(duration.as_millis()).ok())
        .unwrap_or(0)
}

enum TurnDecision {
    Complete(Uuid),
    Continue,
}

#[derive(Clone, Debug, PartialEq)]
pub enum RunOutcome {
    Completed(RunTurnCompleted),
    Continued(RunTurnCompleted),
    Failed(RunTurnFailed),
    Aborted(RunAbortAcknowledged),
}
