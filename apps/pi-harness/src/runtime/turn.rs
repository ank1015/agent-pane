use std::{collections::BTreeMap, time::Duration};

use agent_contracts::{
    HarnessOperation, NewRunMessage, SessionMessage, SessionMessagesAppended, WaitRequest,
};
use agent_harness_sdk::{ActiveTurn, HarnessRuntime, TurnOutcome};
use chrono::Utc;
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
    config::HarnessConfig,
    harness::{
        compact::{
            PiCompactionMessageContent, create_pi_compaction_message, form_compaction_request,
            select_first_kept_message_id,
        },
        context_formation::{ContextFormationError, form_context_messages},
        model_resolver::{RequestKind, get_model_config},
        system_prompt::generate_system_prompt,
        tools::{ToolExecutionContext, WorkspaceCwd, default_tool_definitions, execute_tool_call},
    },
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
    search: tool_firecrawl_search::FirecrawlSearchToolContext,
    scrape: tool_firecrawl_scrape::FirecrawlScrapeToolContext,
    retry: RetryPolicy,
}

impl PiRuntime {
    pub fn from_config(config: &HarnessConfig) -> Result<Self, PiRuntimeBuildError> {
        Ok(Self::new(
            LlmGatewayClient::new(config.llm_gateway.clone())?,
            ExecutionClient::new(config.execution_gateway.clone())?,
            tool_firecrawl_search::FirecrawlSearchToolContext::from_package_env()
                .map_err(PiRuntimeBuildError::SearchTool)?,
            tool_firecrawl_scrape::FirecrawlScrapeToolContext::from_package_env()
                .map_err(PiRuntimeBuildError::ScrapeTool)?,
        ))
    }

    #[must_use]
    pub fn new(
        llm: LlmGatewayClient,
        execution: ExecutionClient,
        search: tool_firecrawl_search::FirecrawlSearchToolContext,
        scrape: tool_firecrawl_scrape::FirecrawlScrapeToolContext,
    ) -> Self {
        Self {
            llm,
            execution,
            search,
            scrape,
            retry: RetryPolicy::pi_default(),
        }
    }

    #[must_use]
    pub fn with_retry_policy(mut self, retry: RetryPolicy) -> Self {
        self.retry = retry;
        self
    }

    pub async fn execute(&self, run: &ActiveTurn) -> Result<TurnOutcome, PiRuntimeError> {
        let result = self.execute_turn(run).await;
        match result {
            Ok(decision) => Ok(self.finish(decision)),
            Err(error) => self.handle_error(run, error),
        }
    }

    async fn execute_turn(&self, run: &ActiveTurn) -> Result<TurnDecision, TurnError> {
        ensure_active(run)?;
        let messages = fetch_messages(run).await?;
        let turn = run.turn_number();
        match plan_turn(&messages, run.run_id(), turn)? {
            ResumePlan::CallModel => {
                let config = PiHarnessConfig::from_resolved(&run.request().resolved_config)?;
                self.call_model(run, &config, messages).await
            }
            ResumePlan::Complete { final_message_id } => {
                Ok(TurnDecision::Complete(final_message_id))
            }
            ResumePlan::ExecuteTools {
                assistant,
                missing_tool_calls,
            } => {
                let config = PiHarnessConfig::from_resolved(&run.request().resolved_config)?;
                self.run_tools(run, &config, &assistant, &missing_tool_calls)
                    .await?;
                Ok(TurnDecision::Continue)
            }
            ResumePlan::Continue => Ok(TurnDecision::Continue),
        }
    }

    async fn call_model(
        &self,
        run: &ActiveTurn,
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
        run: &ActiveTurn,
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
        run: &ActiveTurn,
        config: &PiHarnessConfig,
        assistant: &AssistantMessage,
        tool_calls: &[AssistantContent],
    ) -> Result<(), TurnError> {
        if run.operation().is_cancelled() {
            return Err(TurnError::Cancelled);
        }
        let results = if assistant.stop_reason == StopReason::Length {
            tool_calls
                .iter()
                .map(truncated_tool_result)
                .collect::<Result<Vec<_>, _>>()?
        } else {
            let machine = tokio::select! {
                () = run.operation().cancelled() => {
                    return Err(TurnError::Cancelled);
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
                search: &self.search,
                scrape: &self.scrape,
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

    fn finish(&self, decision: TurnDecision) -> TurnOutcome {
        match decision {
            TurnDecision::Complete(final_message_id) => {
                TurnOutcome::Command(HarnessOperation::Complete { final_message_id })
            }
            TurnDecision::Continue => TurnOutcome::Command(HarnessOperation::Continue),
        }
    }

    fn handle_error(
        &self,
        run: &ActiveTurn,
        error: TurnError,
    ) -> Result<TurnOutcome, PiRuntimeError> {
        if run.operation().is_cancelled() {
            return Ok(TurnOutcome::Cancelled);
        }
        if error.agent_stale() {
            return Ok(TurnOutcome::Stale);
        }
        if error.agent_retryable() {
            let TurnError::Agent(source) = error else {
                unreachable!("agent_retryable only matches Agent")
            };
            return Err(PiRuntimeError::Agent(source));
        }
        if let TurnError::Llm(llm) = &error
            && llm.retryable()
        {
            let delay = self
                .retry
                .delay(self.retry.max_retries.saturating_add(1), llm.retry_after());
            let expires_at = chrono::Duration::from_std(delay)
                .ok()
                .map(|delay| Utc::now() + delay);
            return Ok(TurnOutcome::Command(HarnessOperation::Wait(WaitRequest {
                wait_id: run.request().event_id,
                harness_wait_id: format!("llm-backoff-{}", run.request().event_id),
                kind: "llm_retry".to_owned(),
                public_request: JsonObject::from_iter([
                    ("code".to_owned(), json!(error.code())),
                    ("message".to_owned(), json!(error.to_string())),
                ]),
                resume_metadata: JsonObject::from_iter([(
                    "retry_after_ms".to_owned(),
                    json!(delay.as_millis()),
                )]),
                expires_at,
            })));
        }
        let failure = JsonObject::from_iter([
            ("code".to_owned(), json!(error.code())),
            ("message".to_owned(), json!(error.to_string())),
        ]);
        Ok(TurnOutcome::Command(HarnessOperation::Fail { failure }))
    }
}

#[async_trait::async_trait]
impl HarnessRuntime for PiRuntime {
    type Error = PiRuntimeError;

    async fn execute(&self, turn: &ActiveTurn) -> Result<TurnOutcome, Self::Error> {
        PiRuntime::execute(self, turn).await
    }
}

fn main_request(
    run: &ActiveTurn,
    config: &PiHarnessConfig,
    messages: Vec<Message>,
) -> Result<LlmRequest, TurnError> {
    let model = get_model_config(
        &config.provider,
        &config.model_id,
        &config.reasoning_level,
        RequestKind::Turn {
            session_id: &run.request().session_id.to_string(),
        },
    )?;
    Ok(LlmRequest {
        model: model.model,
        instructions: Some(generate_system_prompt(
            config.external_prompt.as_deref(),
            config.is_replaced,
            config.web_search_enabled,
        )),
        messages,
        tools: default_tool_definitions(config.web_search_enabled),
        provider_options: model.provider_options,
        metadata: BTreeMap::new(),
    })
}

async fn fetch_messages(run: &ActiveTurn) -> Result<Vec<SessionMessage>, TurnError> {
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
    run: &ActiveTurn,
    messages: &[NewRunMessage],
) -> Result<SessionMessagesAppended, TurnError> {
    let mut retries = 0;
    loop {
        let result = run.append_messages(messages).await;
        match result {
            Ok(appended) => return Ok(appended),
            Err(error) if retries < AGENT_MAX_RETRIES && error.retryable() => {
                retries += 1;
                agent_retry_sleep(run.operation(), retries).await?;
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

fn ensure_active(run: &ActiveTurn) -> Result<(), TurnError> {
    if run.operation().is_cancelled() {
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

enum TurnDecision {
    Complete(Uuid),
    Continue,
}
