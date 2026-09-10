use std::{
    future::Future,
    sync::{Arc, Mutex},
    time::Duration,
};
use tool_code_mode::live::{ContentItem, Notification, Session as CodeSession};

use harness_runtime::{Execution, Signals};
use llm_client::{CompletionRequest, IdempotencyKey, RunState};
use llm_contracts::{
    AssistantContent, ContentPart, Message, MessageId, StopReason, TextContent, Timestamp,
    ToolResultError, ToolResultMessage, ToolResultOutcome,
};
use platform_runtime_client::{Command, Error, RequestKey, Result, RunClient, types::*};
use serde_json::{Value, json};
use tokio::sync::watch;
use uuid::Uuid;

use crate::{
    Config, SitesHarness, prompt,
    state::{Phase, State},
    tools::{Output, Plan},
};

const COMMIT_LIMIT: usize = 900 * 1024;

pub(crate) async fn run(harness: SitesHarness, execution: Execution) -> Result<()> {
    let mut context = execution.client.context(&ContextQuery::default()).await?;
    let config = match Config::parse(&context.run.run.config) {
        Ok(config) => config,
        Err(message) => return initial_failure(&execution.client, &context, &message).await,
    };
    if context.run.run.abort_requested_at.is_some() && context.checkpoint.is_none() {
        let mut commit = Commit::new(context.run.run.version, context.session.current_revision);
        commit.disposition = Disposition::Aborted;
        execution.client.commit(&command(commit)?).await?;
        return Ok(());
    }
    let mut messages = std::mem::take(&mut context.messages.items);
    let revision = context.session.current_revision;
    let mut after = context.messages.next_after_revision;
    while let Some(cursor) = after {
        let page = execution
            .client
            .queries()
            .session_messages(
                context.session.id,
                &MessageQuery {
                    after_revision: Some(cursor),
                    limit: Some(200),
                    run_id: None,
                },
            )
            .await?;
        after = page.next_after_revision;
        messages.extend(
            page.items
                .into_iter()
                .filter(|message| message.revision <= revision),
        );
    }
    if messages.iter().any(|entry| match &entry.message {
        Message::ToolResult(result) => matches!(result.tool_name.as_str(), "code_mode" | "inspect_cell" | "reconcile_call"),
        Message::Assistant(message) => message.content.iter().any(|part| matches!(part,
            AssistantContent::ToolCall { name, .. } if matches!(name.as_str(), "code_mode" | "inspect_cell" | "reconcile_call"))),
        _ => false,
    }) {
        return initial_failure(&execution.client, &context, "This session uses the retired Sites tool protocol. Start a new Sites session for exec/wait; existing site data is unchanged.").await;
    }
    let state = if let Some(checkpoint) = &context.checkpoint {
        if checkpoint.state.get("version").and_then(Value::as_u64) != Some(4) {
            return initial_failure(&execution.client, &context, "This checkpoint uses the retired Sites tool surface. Start a new Sites session; existing site data is unchanged.").await;
        }
        let state: State = serde_json::from_value(Value::Object(checkpoint.state.clone()))
            .map_err(|_| Error::Invalid("invalid harness checkpoint"))?;
        if state.version != 4 {
            return Err(Error::Invalid("unsupported harness checkpoint version"));
        }
        state
    } else {
        if context.run.run.abort_requested_at.is_some() {
            let mut commit = Commit::new(context.run.run.version, revision);
            commit.disposition = Disposition::Aborted;
            execution.client.commit(&command(commit)?).await?;
            return Ok(());
        }
        State {
            version: 4,
            instructions: prompt::generate(config.system_prompt_append.as_deref()),
            provider_options: config
                .provider_options(context.session.id)
                .map_err(|_| Error::Invalid("invalid provider policy"))?,
            tools: crate::tools::definitions(),
            phase: Phase::Boundary {
                final_message: None,
            },
            site_id: None,
        }
    };
    let mut driver = Driver {
        harness,
        client: execution.client,
        signals: execution.signals,
        config,
        publish_site: context
            .session
            .harness_contract
            .outputs
            .contains_key("site"),
        code_session: None,
        notifications: None,
        observed_site: Arc::new(Mutex::new(state.site_id)),
        state,
        messages,
        run_id: context.run.run.id,
        session_id: context.session.id,
        run_version: context.run.run.version,
        revision,
        checkpoint_version: context.checkpoint.as_ref().map_or(0, |cp| cp.version),
        abort_requested: context.run.run.abort_requested_at.is_some(),
        finished: false,
    };
    if context.checkpoint.is_none() {
        driver
            .save(Vec::new(), Vec::new(), Disposition::Running)
            .await?;
    }
    driver.drive().await
}

async fn initial_failure(client: &RunClient, context: &RunContext, message: &str) -> Result<()> {
    let mut commit = Commit::new(context.run.run.version, context.session.current_revision);
    commit.disposition = Disposition::Failed {
        error: object(json!({"kind":"configuration","message":message})),
    };
    client.commit(&command(commit)?).await?;
    Ok(())
}

struct Driver {
    harness: SitesHarness,
    client: RunClient,
    signals: watch::Receiver<Signals>,
    config: Config,
    publish_site: bool,
    code_session: Option<CodeSession>,
    notifications: Option<tokio::sync::mpsc::Receiver<Notification>>,
    observed_site: Arc<Mutex<Option<Uuid>>>,
    state: State,
    messages: Vec<SessionMessage>,
    run_id: Uuid,
    session_id: Uuid,
    run_version: i64,
    revision: i64,
    checkpoint_version: i64,
    abort_requested: bool,
    finished: bool,
}

impl Driver {
    async fn drive(&mut self) -> Result<()> {
        loop {
            if self.finished {
                return Ok(());
            }
            if self.signals.has_changed().is_err() {
                return Err(Error::Invalid("worker signal channel closed"));
            }
            let signals = *self.signals.borrow();
            if signals.ownership_lost {
                return Err(Error::Invalid("ownership lost"));
            }
            if signals.abort_requested || self.abort_requested {
                // Signals are observational; the server must confirm the sticky abort.
                self.refresh().await?;
                if self.abort_requested {
                    return self.abort().await;
                }
            }
            if signals.draining {
                self.save(
                    Vec::new(),
                    Vec::new(),
                    Disposition::Ready { available_at: None },
                )
                .await?;
                return Ok(());
            }
            self.flush_notifications().await?;
            match self.state.phase.clone() {
                Phase::Boundary { final_message } => {
                    if self.boundary(final_message).await? {
                        return Ok(());
                    }
                }
                Phase::Model {
                    operation,
                    revision,
                    job,
                    attempt,
                    retry_at_ms,
                } => {
                    if retry_at_ms > now_ms() {
                        let wait = Duration::from_millis((retry_at_ms - now_ms()).min(1000) as u64);
                        guarded(&mut self.signals, tokio::time::sleep(wait)).await;
                        continue;
                    }
                    let llm = self.harness.llm.clone();
                    let result = if let Some(job) = job {
                        guarded(&mut self.signals, llm.get_run(job)).await
                    } else {
                        let request = self.request(revision);
                        let key = IdempotencyKey::new(format!("sites:{}:{operation}", self.run_id))
                            .map_err(|_| Error::Invalid("invalid LLM operation identity"))?;
                        guarded(&mut self.signals, llm.submit(&key, &request)).await
                    };
                    let Some(result) = result else {
                        continue;
                    };
                    let result = match result {
                        Ok(result) => result,
                        Err(llm_client::ClientError::Gateway { failure, status })
                            if status < 500 && !failure.error.can_retry =>
                        {
                            return self.fail("llm_request", &failure.error.message).await;
                        }
                        Err(llm_client::ClientError::InvalidRequest(error)) => {
                            return self.fail("llm_request", &error.to_string()).await;
                        }
                        Err(_) => {
                            return Err(Error::Invalid(
                                "LLM transport/retrieval failed; recover the saved operation, do not resubmit with a new key",
                            ));
                        }
                    };
                    if job.is_none() {
                        self.state.phase = Phase::Model {
                            operation,
                            revision,
                            job: Some(result.run_id),
                            attempt,
                            retry_at_ms: 0,
                        };
                        self.save(Vec::new(), Vec::new(), Disposition::Running)
                            .await?;
                    }
                    // Abort/drain may have arrived while recording the gateway handle.
                    if self.interrupted() {
                        continue;
                    }
                    match result.state {
                        RunState::Running => {
                            guarded(
                                &mut self.signals,
                                tokio::time::sleep(Duration::from_millis(500)),
                            )
                            .await;
                        }
                        RunState::Succeeded(completion) => {
                            self.assistant(completion.message).await?
                        }
                        RunState::Failed(failure) if failure.error.can_retry && attempt < 2 => {
                            let delay = failure.error.retry_after_ms.unwrap_or(1000 << attempt);
                            let delay = i64::try_from(delay).unwrap_or(i64::MAX);
                            self.state.phase = Phase::Model {
                                operation: Uuid::new_v4(),
                                revision,
                                job: None,
                                attempt: attempt + 1,
                                retry_at_ms: now_ms().saturating_add(delay),
                            };
                            self.save(Vec::new(), Vec::new(), Disposition::Running)
                                .await?;
                        }
                        RunState::Failed(failure) => {
                            return self.fail("llm_failure", &failure.error.message).await;
                        }
                        RunState::Expired => return self
                            .fail(
                                "llm_expired",
                                "Saved LLM result expired; no automatic new generation was started",
                            )
                            .await,
                        RunState::Aborted => {
                            return self
                                .fail("llm_aborted", "LLM generation was aborted outside this run")
                                .await;
                        }
                    }
                }
                Phase::Tools {
                    assistant,
                    index,
                    plan,
                } => self.tool(assistant, index, plan).await?,
            }
        }
    }

    fn interrupted(&self) -> bool {
        let signal = *self.signals.borrow();
        signal.abort_requested || signal.draining || signal.ownership_lost || self.abort_requested
    }

    async fn boundary(&mut self, final_message: Option<Uuid>) -> Result<bool> {
        let inputs = self
            .client
            .inputs(&SequenceQuery {
                limit: Some(100),
                after_sequence: None,
                status: Some("pending".into()),
            })
            .await?;
        if !inputs.items.is_empty() {
            let mut messages = Vec::new();
            let mut results = Vec::new();
            let mut message_bytes = 0;
            for input in inputs.items {
                if input.kind == "abort" {
                    self.refresh().await?;
                    if self.abort_requested {
                        return Ok(false);
                    }
                }
                let message = if input.kind == "user_message" {
                    input
                        .payload
                        .get("message")
                        .cloned()
                        .and_then(|value| serde_json::from_value::<Message>(value).ok())
                        .filter(|message| matches!(message, Message::User(_)))
                } else {
                    None
                };
                let size = message
                    .as_ref()
                    .map_or(0, |message| serde_json::to_vec(message).unwrap().len());
                if size <= 512 * 1024 && message_bytes + size > 512 * 1024 {
                    break;
                }
                let message = message.filter(|_| size <= 512 * 1024);
                let accepted = message.is_some();
                let message_id = accepted.then(Uuid::now_v7);
                if accepted {
                    message_bytes += size;
                }
                if let Some(message) = message {
                    messages.push(AppendMessage {
                        message_id: message_id.unwrap(),
                        message,
                    });
                }
                results.push(InputResult { id: input.id, status: if accepted { InputStatus::Handled } else { InputStatus::Rejected },
                    handling: object(json!({"message_id":message_id,"reason": if accepted { "appended_to_history" } else { "unsupported input kind or user message exceeds 512 KiB" }})) });
            }
            if !messages.is_empty() {
                self.state.phase = Phase::Boundary {
                    final_message: None,
                };
            }
            self.save(messages, results, Disposition::Running).await?;
            return Ok(false);
        }
        if self.interrupted() {
            return Ok(false);
        }
        if self.messages.is_empty() {
            self.fail(
                "invalid_input",
                "No accepted conversation messages to process",
            )
            .await?;
            return Ok(true);
        }
        if let Some(final_message_id) = final_message {
            match self
                .save(
                    Vec::new(),
                    Vec::new(),
                    Disposition::Completed { final_message_id },
                )
                .await
            {
                Ok(()) => return Ok(true),
                Err(error) if error.conflict() == Some(ConflictCode::InputStateConflict) => {
                    self.refresh().await?;
                    return Ok(false);
                }
                Err(error) => return Err(error),
            }
        }
        self.state.phase = Phase::Model {
            operation: Uuid::new_v4(),
            revision: self.revision,
            job: None,
            attempt: 0,
            retry_at_ms: 0,
        };
        self.save(Vec::new(), Vec::new(), Disposition::Running)
            .await?;
        Ok(false)
    }

    fn request(&self, revision: i64) -> CompletionRequest {
        CompletionRequest {
            account_id: self.config.account_id,
            request: llm_contracts::LlmRequest {
                model: self.config.model.clone(),
                instructions: Some(self.state.instructions.clone()),
                messages: self
                    .messages
                    .iter()
                    .filter(|message| message.revision <= revision)
                    .map(|message| message.message.clone())
                    .collect(),
                tools: self.state.tools.clone(),
                provider_options: self.state.provider_options.clone(),
                metadata: [
                    ("platform_run_id".into(), self.run_id.to_string()),
                    ("session_id".into(), self.session_id.to_string()),
                ]
                .into_iter()
                .collect(),
            },
        }
    }

    async fn assistant(&mut self, assistant: llm_contracts::AssistantMessage) -> Result<()> {
        // Provider payloads remain intact; don't rebuild native reasoning/tool items.
        if serde_json::to_vec(&assistant).unwrap().len() > 512 * 1024 {
            return self
                .fail(
                    "message_too_large",
                    "Assistant response exceeds the harness's 512 KiB message-storage limit",
                )
                .await;
        }
        let id = Uuid::now_v7();
        let call_ids: Vec<_> = assistant
            .content
            .iter()
            .filter_map(|part| match part {
                AssistantContent::ToolCall { tool_call_id, .. } => Some(tool_call_id.as_str()),
                _ => None,
            })
            .collect();
        let unique: std::collections::HashSet<_> = call_ids.iter().collect();
        if call_ids.len() > 100 || unique.len() != call_ids.len() {
            return self
                .fail(
                    "invalid_tool_batch",
                    "Model returned duplicate tool call IDs or more than 100 calls in one response",
                )
                .await;
        }
        let has_tools = assistant
            .content
            .iter()
            .any(|part| matches!(part, AssistantContent::ToolCall { .. }));
        let final_text = assistant.content.iter().any(|part| matches!(part, AssistantContent::Response { response } if !response.content.trim().is_empty()));
        let valid = if has_tools {
            matches!(
                assistant.stop_reason,
                StopReason::Stop | StopReason::ToolUse
            )
        } else {
            final_text
                && matches!(
                    assistant.stop_reason,
                    StopReason::Stop | StopReason::Refusal
                )
        };
        self.state.phase = if has_tools {
            Phase::Tools {
                assistant: id,
                index: 0,
                plan: None,
            }
        } else {
            Phase::Boundary {
                final_message: Some(id),
            }
        };
        self.save(
            vec![AppendMessage {
                message_id: id,
                message: Message::Assistant(assistant),
            }],
            Vec::new(),
            Disposition::Running,
        )
        .await?;
        if !valid {
            self.fail(
                "incomplete_response",
                "Model response was truncated, filtered, empty, or had an unsupported stop reason",
            )
            .await?;
        }
        Ok(())
    }

    fn calls(&self, assistant: Uuid) -> Result<Vec<AssistantContent>> {
        let Some(Message::Assistant(message)) = self
            .messages
            .iter()
            .find(|message| message.message_id == assistant)
            .map(|message| &message.message)
        else {
            return Err(Error::Invalid("checkpoint assistant message missing"));
        };
        Ok(message
            .content
            .iter()
            .filter(|part| matches!(part, AssistantContent::ToolCall { .. }))
            .cloned()
            .collect())
    }

    async fn tool(&mut self, assistant: Uuid, index: usize, plan: Option<Box<Plan>>) -> Result<()> {
        let calls = self.calls(assistant)?;
        let Some(call) = calls.get(index) else {
            self.state.phase = Phase::Boundary {
                final_message: None,
            };
            return self
                .save(Vec::new(), Vec::new(), Disposition::Running)
                .await;
        };
        let plan = match plan {
            Some(plan) => *plan,
            None => {
                let AssistantContent::ToolCall {
                    name, arguments, ..
                } = call
                else {
                    unreachable!()
                };
                match crate::tools::prepare(name, arguments) {
                    Ok(plan) => {
                        self.state.phase = Phase::Tools {
                            assistant,
                            index,
                            plan: Some(Box::new(plan)),
                        };
                        return self
                            .save(Vec::new(), Vec::new(), Disposition::Running)
                            .await;
                    }
                    Err(message) => {
                        return self
                            .tool_result(assistant, index, call, Output::error(message))
                            .await;
                    }
                }
            }
        };
        if let Plan::Exec { admitted: true, .. } = &plan {
            return self.tool_result(assistant, index, call, Output::error(
                "Script interrupted by loss of the runtime. Source was not replayed. Inspect downstream operation handles before repeating effects."
            )).await;
        }
        if let Plan::Exec { input, .. } = &plan {
            // Commit before dispatch. Recovery of this marker must never replay arbitrary tools.
            self.state.phase = Phase::Tools {
                assistant,
                index,
                plan: Some(Box::new(Plan::Exec {
                    input: input.clone(),
                    admitted: true,
                })),
            };
            self.save(Vec::new(), Vec::new(), Disposition::Running)
                .await?;
        }
        if self.code_session.is_none() {
            let (session, notifications) = self
                .harness
                .code_mode(self.client.clone(), self.observed_site.clone())?;
            self.code_session = Some(session);
            self.notifications = Some(notifications);
        }
        let mut session = self.code_session.take().unwrap();
        let mut notifications = self.notifications.take().unwrap();
        let AssistantContent::ToolCall { tool_call_id, .. } = call else {
            unreachable!()
        };
        let mut operation = Box::pin(async {
            match plan {
                Plan::Exec { input, .. } => session.exec(tool_call_id.as_str().into(), input).await,
                Plan::Wait { input } => session.wait(input).await,
            }
        });
        let mut signals = self.signals.clone();
        let report = loop {
            if self.interrupted() {
                break None;
            }
            tokio::select! {
                result = &mut operation => break Some(result),
                notification = notifications.recv() => {
                    if let Some(notification) = notification { self.save_notification(notification).await?; }
                }
                changed = signals.changed() => {
                    if changed.is_err() || self.interrupted() { break None; }
                }
            }
        };
        // Drop the borrow before restoring the session. On interruption its processes are cancelled.
        drop(operation);
        let output = match report {
            Some(Ok(report)) => Output::report(report),
            Some(Err(message)) => Output::error(message),
            None => Output::error("Script interrupted; accepted nested effects may still exist."),
        };
        self.notifications = Some(notifications);
        self.code_session = Some(session);
        if self.interrupted() {
            self.code_session = None;
        }
        self.state.site_id = *self.observed_site.lock().unwrap();
        self.refresh().await?;
        self.tool_result(assistant, index, call, output).await
    }

    async fn save_notification(&mut self, notification: Notification) -> Result<()> {
        let call = self.messages.iter().rev().find_map(|message| {
            let Message::Assistant(message) = &message.message else { return None };
            message.content.iter().find(|content| matches!(content,
                AssistantContent::ToolCall { tool_call_id, .. } if tool_call_id.as_str() == notification.call_id)).cloned()
        }).ok_or(Error::Invalid("Notification references an unknown exec call"))?;
        self.refresh().await?;
        self.save(
            vec![tool_message(&call, notification.text, false)],
            Vec::new(),
            Disposition::Running,
        )
        .await
    }

    async fn flush_notifications(&mut self) -> Result<()> {
        loop {
            let notification = self
                .notifications
                .as_mut()
                .and_then(|receiver| receiver.try_recv().ok());
            let Some(notification) = notification else {
                return Ok(());
            };
            self.save_notification(notification).await?;
        }
    }

    async fn tool_result(
        &mut self,
        assistant: Uuid,
        index: usize,
        call: &AssistantContent,
        output: Output,
    ) -> Result<()> {
        self.state.phase = Phase::Tools {
            assistant,
            index: index + 1,
            plan: None,
        };
        self.save(
            vec![tool_output_message(call, output)],
            Vec::new(),
            Disposition::Running,
        )
        .await
    }

    async fn abort(&mut self) -> Result<()> {
        self.code_session = None;
        if let Phase::Model {
            job,
            operation,
            revision,
            ..
        } = self.state.phase.clone()
        {
            // Recover an ambiguous submit with the SAME identity/payload, never
            // a new generation. Persist the recovered handle before aborting it.
            let job = match job {
                Some(job) => job,
                None => {
                    let key = IdempotencyKey::new(format!("sites:{}:{operation}", self.run_id))
                        .map_err(|_| Error::Invalid("invalid LLM identity"))?;
                    let job = self
                        .harness
                        .llm
                        .submit(&key, &self.request(revision))
                        .await
                        .map_err(|_| Error::Invalid("could not reconcile LLM operation for abort"))?
                        .run_id;
                    if let Phase::Model { job: saved, .. } = &mut self.state.phase {
                        *saved = Some(job);
                    }
                    self.save(Vec::new(), Vec::new(), Disposition::Running)
                        .await?;
                    job
                }
            };
            self.harness.llm.abort(job).await.map_err(|_| {
                Error::Invalid("LLM abort outcome uncertain; recover and reconcile")
            })?;
        }
        let messages = self.unfinished_tools(
            "Run aborted. Tool execution was cancelled or its interrupted outcome is unknown.",
        )?;
        let inputs = self
            .client
            .inputs(&SequenceQuery {
                limit: Some(200),
                after_sequence: None,
                status: Some("pending".into()),
            })
            .await?;
        let results = inputs
            .items
            .into_iter()
            .map(|input| InputResult {
                id: input.id,
                status: if input.kind == "abort" {
                    InputStatus::Handled
                } else {
                    InputStatus::Rejected
                },
                handling: object(json!({"reason":"run_aborted"})),
            })
            .collect();
        self.state.phase = Phase::Boundary {
            final_message: None,
        };
        self.save(messages, results, Disposition::Aborted).await
    }

    fn unfinished_tools(&self, text: &str) -> Result<Vec<AppendMessage>> {
        if let Phase::Tools {
            assistant, index, ..
        } = self.state.phase
        {
            return Ok(self
                .calls(assistant)?
                .iter()
                .skip(index)
                .map(|call| tool_message(call, text.into(), true))
                .collect());
        }
        Ok(Vec::new())
    }

    async fn fail(&mut self, kind: &str, message: &str) -> Result<()> {
        let messages = self.unfinished_tools("Run failed before this tool result was committed; any interrupted side effect must be checked before retrying.")?;
        self.state.phase = Phase::Boundary {
            final_message: None,
        };
        self.save(
            messages,
            Vec::new(),
            Disposition::Failed {
                error: object(json!({"kind":kind,"message":message})),
            },
        )
        .await
    }

    async fn refresh(&mut self) -> Result<()> {
        let context = self
            .client
            .context(&ContextQuery {
                limit: Some(1),
                ..Default::default()
            })
            .await?;
        if context.session.current_revision != self.revision
            || context.checkpoint.as_ref().map_or(0, |cp| cp.version) != self.checkpoint_version
        {
            return Err(Error::Invalid("durable state changed; reload activation"));
        }
        self.run_version = context.run.run.version;
        self.abort_requested = context.run.run.abort_requested_at.is_some();
        Ok(())
    }

    async fn save(
        &mut self,
        messages: Vec<AppendMessage>,
        inputs: Vec<InputResult>,
        disposition: Disposition,
    ) -> Result<()> {
        if self.publish_site && matches!(disposition, Disposition::Completed { .. }) {
            if let Some(site_id) = self.state.site_id {
                self.client
                    .publish_output(&Command::new(
                        RequestKey::new("sites-output-v1")?,
                        PublishRunOutput {
                            name: "site".into(),
                            output: OutputValue::Json(json!({"siteId":site_id})),
                        },
                    ))
                    .await?;
                self.refresh().await?;
            }
        }
        let mut commit = Commit::new(self.run_version, self.revision);
        commit.checkpoint = Some(Checkpoint {
            expected_version: self.checkpoint_version,
            state: object(json!(self.state)),
        });
        commit.messages = messages;
        commit.input_results = inputs;
        commit.disposition = disposition;
        if serde_json::to_vec(&commit).unwrap().len() > COMMIT_LIMIT {
            return Err(Error::Invalid(
                "harness commit exceeds bounded storage budget",
            ));
        }
        let mut command = command(commit)?;
        let mut retries = 0;
        let reply = loop {
            match self.client.commit(&command).await {
                Ok(reply) => break reply,
                Err(error)
                    if error.conflict() == Some(ConflictCode::RunVersionConflict)
                        && retries < 3 =>
                {
                    // Only retry a confirmed rejection, after checking checkpoint/history
                    // did not change. Ambiguous transport errors retain the original body.
                    self.refresh().await?;
                    let mut body: Commit = serde_json::from_value(json!(command.body())).unwrap();
                    body.expected_run_version = self.run_version;
                    command = crate::driver::command(body)?;
                    retries += 1;
                }
                Err(error) => return Err(error),
            }
        };
        for (offset, message) in command.body().messages.iter().enumerate() {
            self.messages.push(SessionMessage {
                message_id: message.message_id,
                revision: self.revision + offset as i64 + 1,
                run_id: Some(self.run_id),
                origin_run_id: Some(self.run_id),
                message: message.message.clone(),
                created_at: chrono::Utc::now(),
            });
        }
        self.run_version = reply.run.run.version;
        self.finished = matches!(
            reply.run.run.status,
            RunStatus::Completed | RunStatus::Failed | RunStatus::Aborted
        );
        self.abort_requested = reply.run.run.abort_requested_at.is_some();
        self.revision = reply.session_revision;
        self.checkpoint_version = reply
            .checkpoint
            .as_ref()
            .map_or(self.checkpoint_version, |cp| cp.version);
        Ok(())
    }
}

fn tool_message(call: &AssistantContent, text: String, error: bool) -> AppendMessage {
    tool_output_message(
        call,
        Output {
            content: vec![ContentItem::InputText { text }],
            error,
        },
    )
}

fn tool_output_message(call: &AssistantContent, output: Output) -> AppendMessage {
    let AssistantContent::ToolCall {
        name, tool_call_id, ..
    } = call
    else {
        unreachable!()
    };
    let id = Uuid::now_v7();
    AppendMessage {
        message_id: id,
        message: Message::ToolResult(ToolResultMessage {
            id: MessageId::new(id.to_string()).unwrap(),
            tool_name: name.clone(),
            tool_call_id: tool_call_id.clone(),
            content: output
                .content
                .into_iter()
                .map(|item| match item {
                    ContentItem::InputText { text } => ContentPart::Text(TextContent {
                        content: text,
                        metadata: None,
                    }),
                    ContentItem::InputAudio { audio_url } => {
                        ContentPart::Audio(llm_contracts::AudioContent { audio_url })
                    }
                    ContentItem::InputImage { image_url, detail } => {
                        let (mime, data) = image_url
                            .strip_prefix("data:")
                            .and_then(|url| url.split_once(";base64,"))
                            .unwrap_or(("image/png", ""));
                        ContentPart::Image(llm_contracts::ImageContent {
                            source: llm_contracts::ImageSource::Base64(
                                llm_contracts::Base64ImageSource {
                                    mime_type: mime.into(),
                                    data: data.into(),
                                },
                            ),
                            detail: detail
                                .and_then(|detail| serde_json::from_value(json!(detail)).ok()),
                            metadata: None,
                        })
                    }
                })
                .collect(),
            details: None,
            timestamp: Timestamp(now_ms() as u64),
            outcome: if output.error {
                ToolResultOutcome::Error {
                    error: ToolResultError {
                        message: "Tool execution failed; see result content".into(),
                        name: Some("ToolError".into()),
                    },
                }
            } else {
                ToolResultOutcome::Success
            },
        }),
    }
}

async fn guarded<T>(
    signals: &mut watch::Receiver<Signals>,
    future: impl Future<Output = T>,
) -> Option<T> {
    tokio::pin!(future);
    loop {
        let signal = *signals.borrow();
        if signal.abort_requested || signal.draining || signal.ownership_lost {
            return None;
        }
        tokio::select! {
            result = &mut future => return Some(result),
            changed = signals.changed() => { if changed.is_err() { return None; } },
        }
    }
}
pub(crate) fn command(commit: Commit) -> Result<Command<Commit>> {
    Ok(Command::new(
        RequestKey::new(Uuid::new_v4().to_string())?,
        commit,
    ))
}
fn object(value: Value) -> llm_contracts::JsonObject {
    value.as_object().expect("serialized object").clone()
}
fn now_ms() -> i64 {
    chrono::Utc::now().timestamp_millis()
}
