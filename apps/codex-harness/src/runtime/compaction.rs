use agent_contracts::SessionMessage;
use llm_contracts::{
    AssistantContent, AssistantMessage, ContentPart, CustomMessage, LlmError, LlmRequest, Message,
    MessageId, TextContent, Timestamp, ToolResultError, ToolResultMessage, ToolResultOutcome,
    Usage, Validate, ValidationError,
};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use uuid::Uuid;

use super::{CodexEnvironmentSnapshot, CodexHarnessConfig, CodexProvider, form_main_request};

pub const CODEX_COMPACTION_MESSAGE_TAG: &str = "codex.compaction";
pub const CODEX_COMPACTION_SCHEMA_VERSION: u32 = 1;
pub const RETAINED_MESSAGE_TOKEN_BUDGET: u64 = 64_000;

const ESTIMATED_IMAGE_TOKENS: u64 = 1_200;

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum CodexCompactionTrigger {
    AutomaticLimit,
    ContextOverflow,
}

/// Durable replacement-history checkpoint produced by Responses compaction.
/// `items` contains the opaque provider item, never a plaintext summary.
#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct CodexCompactionMessageContent {
    pub version: u32,
    pub provider: CodexProvider,
    pub trigger: CodexCompactionTrigger,
    pub retained_messages: Vec<Message>,
    pub items: Vec<Value>,
    pub active_context_tokens: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub usage: Option<Usage>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum CompactionPlan {
    NotRequired {
        estimated_tokens: u64,
        token_limit: u64,
    },
    Compact {
        trigger: CodexCompactionTrigger,
        estimated_tokens: u64,
        token_limit: u64,
    },
    AlreadyCompactedForTurn {
        estimated_tokens: u64,
        token_limit: u64,
    },
}

pub fn create_codex_compaction_message(
    id: MessageId,
    timestamp: Timestamp,
    compaction: CodexCompactionMessageContent,
) -> Result<CustomMessage, serde_json::Error> {
    let content = serde_json::to_value(compaction)?;
    let Value::Object(content) = content else {
        unreachable!("CodexCompactionMessageContent serializes as an object")
    };
    Ok(CustomMessage {
        id,
        content,
        tag: Some(CODEX_COMPACTION_MESSAGE_TAG.to_owned()),
        timestamp,
    })
}

/// Forms a Responses-compaction-v2 request: the normal active request followed
/// by the provider-native `compaction_trigger` input item.
pub fn form_compaction_request(
    config: &CodexHarnessConfig,
    session_id: Uuid,
    environment: &CodexEnvironmentSnapshot,
    session_messages: &[SessionMessage],
) -> Result<LlmRequest, CompactionError> {
    let mut request = form_main_request(config, session_id, environment, session_messages)?;
    request.messages.push(Message::Custom(CustomMessage {
        id: MessageId::new(format!("codex-compaction-trigger-{session_id}"))
            .expect("UUID-derived message ID is valid"),
        content: json!({ "items": [{ "type": "compaction_trigger" }] })
            .as_object()
            .expect("static value is an object")
            .clone(),
        tag: Some(native_input_tag(config.provider).to_owned()),
        timestamp: environment.captured_at,
    }));
    request
        .validate()
        .map_err(CompactionError::InvalidRequest)?;
    Ok(request)
}

/// Converts the compaction response into an append-only Agent checkpoint that
/// acts as a replacement-history boundary on later requests.
pub fn checkpoint_from_response(
    config: &CodexHarnessConfig,
    id: MessageId,
    timestamp: Timestamp,
    trigger: CodexCompactionTrigger,
    active_messages: &[Message],
    response: &AssistantMessage,
) -> Result<CustomMessage, CompactionError> {
    if response.model.provider.as_str() != config.provider.as_str() {
        return Err(CompactionError::ResponseProviderMismatch {
            expected: config.provider.as_str().to_owned(),
            actual: response.model.provider.as_str().to_owned(),
        });
    }
    let output = response
        .native_message
        .get("output")
        .and_then(Value::as_array)
        .ok_or(CompactionError::MissingResponseOutput)?;
    let compaction_items = output
        .iter()
        .filter(|item| {
            matches!(
                item.get("type").and_then(Value::as_str),
                Some("compaction" | "compaction_summary")
            )
        })
        .cloned()
        .collect::<Vec<_>>();
    if compaction_items.len() != 1 {
        return Err(CompactionError::UnexpectedCompactionItemCount(
            compaction_items.len(),
        ));
    }
    if compaction_items[0]
        .get("encrypted_content")
        .and_then(Value::as_str)
        .is_none_or(str::is_empty)
    {
        return Err(CompactionError::MissingEncryptedContent);
    }

    let retained_messages = select_retained_messages(active_messages);
    let active_context_tokens = retained_messages
        .iter()
        .map(estimate_message_tokens)
        .sum::<u64>()
        .saturating_add(estimate_json_tokens(&compaction_items[0]));
    create_codex_compaction_message(
        id,
        timestamp,
        CodexCompactionMessageContent {
            version: CODEX_COMPACTION_SCHEMA_VERSION,
            provider: config.provider,
            trigger,
            retained_messages,
            items: compaction_items,
            active_context_tokens,
            usage: response.usage.clone(),
        },
    )
    .map_err(CompactionError::SerializeCheckpoint)
}

/// Mirrors Codex's pre-sampling threshold. `prior_context_overflow` means an
/// earlier sampling boundary marked the context full; Codex returns that error
/// and compacts only when the next pre-sampling boundary invokes this planner.
/// A committed checkpoint prevents redelivery from compacting that boundary
/// again.
#[must_use]
pub fn plan_compaction(
    request: &LlmRequest,
    session_messages: &[SessionMessage],
    model_context_window: u64,
    prior_context_overflow: bool,
    run_id: Uuid,
    turn_number: u32,
) -> CompactionPlan {
    let estimated_tokens = estimate_request_tokens(request);
    let token_limit = model_context_window.saturating_mul(9) / 10;
    if prior_context_overflow {
        if has_compacted_for_turn(session_messages, run_id, turn_number) {
            return CompactionPlan::AlreadyCompactedForTurn {
                estimated_tokens,
                token_limit,
            };
        }
        return CompactionPlan::Compact {
            trigger: CodexCompactionTrigger::ContextOverflow,
            estimated_tokens,
            token_limit,
        };
    }
    if estimated_tokens >= token_limit {
        CompactionPlan::Compact {
            trigger: CodexCompactionTrigger::AutomaticLimit,
            estimated_tokens,
            token_limit,
        }
    } else {
        CompactionPlan::NotRequired {
            estimated_tokens,
            token_limit,
        }
    }
}

#[must_use]
pub fn is_context_overflow(error: &LlmError) -> bool {
    error
        .provider_code
        .as_deref()
        .is_some_and(|code| code.eq_ignore_ascii_case("context_length_exceeded"))
        || error
            .provider_type
            .as_deref()
            .is_some_and(|kind| kind.eq_ignore_ascii_case("context_length_exceeded"))
}

/// Canonicalizes Agent history and expands the newest durable checkpoint into
/// retained messages followed by its provider-native opaque compaction block.
pub fn normalize_session_messages(
    session_id: Uuid,
    messages: &[SessionMessage],
    provider: CodexProvider,
) -> Result<Vec<Message>, ContextNormalizationError> {
    let mut ordered = messages.iter().collect::<Vec<_>>();
    ordered.sort_by_key(|message| message.revision);
    for message in &ordered {
        if message.session_id != session_id {
            return Err(ContextNormalizationError::SessionMismatch {
                expected: session_id,
                actual: message.session_id,
            });
        }
    }
    for pair in ordered.windows(2) {
        if pair[0].revision == pair[1].revision {
            return Err(ContextNormalizationError::DuplicateRevision(
                pair[0].revision,
            ));
        }
    }

    let messages = ordered
        .into_iter()
        .map(|message| &message.message)
        .collect::<Vec<_>>();
    let mut normalized = Vec::new();
    if let Some((checkpoint_index, checkpoint_message, checkpoint)) = latest_checkpoint(&messages)?
    {
        validate_checkpoint(&checkpoint, provider)?;
        normalized.extend(checkpoint.retained_messages);
        normalized.push(native_checkpoint_message(
            checkpoint_message,
            provider,
            checkpoint.items,
        ));
        extend_visible(&mut normalized, &messages[checkpoint_index + 1..], provider);
    } else {
        extend_visible(&mut normalized, &messages, provider);
    }
    repair_interrupted_tool_calls(&mut normalized);
    Ok(normalized)
}

/// Responses requires every emitted tool call to be followed by exactly one
/// output before another conversational item. A run can be aborted after its
/// assistant response commits but before tool execution commits. Preserve the
/// assistant turn and make that durable history replayable by synthesizing the
/// model-visible cancellation output that the interrupted execution could not
/// append to the now-terminal Agent run.
fn repair_interrupted_tool_calls(messages: &mut Vec<Message>) {
    let mut repaired = Vec::with_capacity(messages.len());
    let mut pending = Vec::<(String, llm_contracts::ToolCallId, Timestamp)>::new();

    for message in messages.drain(..) {
        match &message {
            Message::ToolResult(result) => {
                pending.retain(|(_, tool_call_id, _)| tool_call_id != &result.tool_call_id);
            }
            Message::Assistant(_) | Message::User(_) | Message::System(_) | Message::Custom(_) => {
                flush_interrupted_tool_results(&mut repaired, &mut pending);
            }
        }
        if let Message::Assistant(assistant) = &message {
            pending.extend(assistant.content.iter().filter_map(|content| {
                let AssistantContent::ToolCall {
                    name, tool_call_id, ..
                } = content
                else {
                    return None;
                };
                Some((name.clone(), tool_call_id.clone(), assistant.timestamp))
            }));
        }
        repaired.push(message);
    }
    flush_interrupted_tool_results(&mut repaired, &mut pending);
    *messages = repaired;
}

fn flush_interrupted_tool_results(
    messages: &mut Vec<Message>,
    pending: &mut Vec<(String, llm_contracts::ToolCallId, Timestamp)>,
) {
    messages.extend(
        pending
            .drain(..)
            .map(|(tool_name, tool_call_id, timestamp)| {
                Message::ToolResult(ToolResultMessage {
                    id: MessageId::new(format!("codex-interrupted-tool-{tool_call_id}"))
                        .expect("tool-call-derived message ID is valid"),
                    tool_name,
                    tool_call_id,
                    content: vec![ContentPart::Text(TextContent {
                        content: "Tool execution was interrupted before a result was committed."
                            .to_owned(),
                        metadata: None,
                    })],
                    details: None,
                    timestamp,
                    outcome: ToolResultOutcome::Error {
                        error: ToolResultError {
                            message:
                                "Tool execution was interrupted before a result was committed."
                                    .to_owned(),
                            name: Some("cancelled".to_owned()),
                        },
                    },
                })
            }),
    );
}

fn latest_checkpoint<'a>(
    messages: &'a [&Message],
) -> Result<
    Option<(usize, &'a CustomMessage, CodexCompactionMessageContent)>,
    ContextNormalizationError,
> {
    let Some((index, Message::Custom(message))) =
        messages
            .iter()
            .enumerate()
            .rev()
            .find_map(|(index, message)| {
                matches!(
                    message,
                    Message::Custom(custom)
                        if custom.tag.as_deref() == Some(CODEX_COMPACTION_MESSAGE_TAG)
                )
                .then_some((index, *message))
            })
    else {
        return Ok(None);
    };
    let checkpoint = serde_json::from_value(Value::Object(message.content.clone()))
        .map_err(ContextNormalizationError::InvalidCompactionMessage)?;
    Ok(Some((index, message, checkpoint)))
}

fn validate_checkpoint(
    checkpoint: &CodexCompactionMessageContent,
    provider: CodexProvider,
) -> Result<(), ContextNormalizationError> {
    if checkpoint.version != CODEX_COMPACTION_SCHEMA_VERSION {
        return Err(ContextNormalizationError::UnsupportedVersion(
            checkpoint.version,
        ));
    }
    if checkpoint.provider != provider {
        return Err(ContextNormalizationError::ProviderMismatch {
            checkpoint: checkpoint.provider,
            request: provider,
        });
    }
    if checkpoint.items.len() != 1
        || checkpoint.items[0]
            .get("type")
            .and_then(Value::as_str)
            .is_none_or(|kind| !matches!(kind, "compaction" | "compaction_summary"))
    {
        return Err(ContextNormalizationError::InvalidCompactionItems);
    }
    if checkpoint
        .retained_messages
        .iter()
        .any(|message| !matches!(message, Message::User(_) | Message::System(_)))
    {
        return Err(ContextNormalizationError::InvalidRetainedMessage);
    }
    Ok(())
}

fn native_checkpoint_message(
    checkpoint: &CustomMessage,
    provider: CodexProvider,
    items: Vec<Value>,
) -> Message {
    Message::Custom(CustomMessage {
        id: MessageId::new(format!("{}-native", checkpoint.id))
            .expect("checkpoint-derived message ID is valid"),
        content: json!({ "items": items })
            .as_object()
            .expect("static value is an object")
            .clone(),
        tag: Some(native_input_tag(provider).to_owned()),
        timestamp: checkpoint.timestamp,
    })
}

fn select_retained_messages(messages: &[Message]) -> Vec<Message> {
    let mut remaining = RETAINED_MESSAGE_TOKEN_BUDGET;
    let mut retained = Vec::new();
    for message in messages.iter().rev() {
        if !matches!(message, Message::User(_) | Message::System(_))
            || is_ephemeral_environment(message)
        {
            continue;
        }
        let tokens = estimate_message_tokens(message).max(1);
        if tokens <= remaining {
            retained.push(message.clone());
            remaining = remaining.saturating_sub(tokens);
        }
        if remaining == 0 {
            break;
        }
    }
    retained.reverse();
    retained
}

fn is_ephemeral_environment(message: &Message) -> bool {
    matches!(
        message,
        Message::User(message) if message.id.as_str().starts_with("codex-environment-")
    )
}

fn has_compacted_for_turn(messages: &[SessionMessage], run_id: Uuid, turn_number: u32) -> bool {
    messages.iter().any(|message| {
        message.run_id == Some(run_id)
            && message.turn_number == Some(turn_number)
            && matches!(
                &message.message,
                Message::Custom(custom)
                    if custom.tag.as_deref() == Some(CODEX_COMPACTION_MESSAGE_TAG)
            )
    })
}

fn extend_visible(target: &mut Vec<Message>, messages: &[&Message], provider: CodexProvider) {
    target.extend(messages.iter().filter_map(|message| match message {
        Message::Custom(custom) if custom.tag.as_deref() == Some(native_input_tag(provider)) => {
            Some((*message).clone())
        }
        Message::Custom(_) => None,
        _ => Some((*message).clone()),
    }));
}

fn native_input_tag(provider: CodexProvider) -> &'static str {
    match provider {
        CodexProvider::OpenAi => provider_openai::OPENAI_NATIVE_INPUT_TAG,
        CodexProvider::ChatGpt => provider_chatgpt::CHATGPT_NATIVE_INPUT_TAG,
    }
}

fn estimate_request_tokens(request: &LlmRequest) -> u64 {
    if let Some((assistant_index, usage)) =
        request
            .messages
            .iter()
            .enumerate()
            .rev()
            .find_map(|(index, message)| match message {
                Message::Assistant(message) => message.usage.as_ref().map(|usage| (index, usage)),
                _ => None,
            })
    {
        return usage_context_tokens(usage).saturating_add(
            request.messages[assistant_index + 1..]
                .iter()
                .map(estimate_message_tokens)
                .fold(0_u64, u64::saturating_add),
        );
    }
    let instructions = request
        .instructions
        .as_deref()
        .map_or(0, estimate_text_tokens);
    let tools =
        serde_json::to_value(&request.tools).map_or(0, |tools| estimate_json_tokens(&tools));
    request
        .messages
        .iter()
        .fold(instructions.saturating_add(tools), |total, message| {
            total.saturating_add(estimate_message_tokens(message))
        })
}

fn usage_context_tokens(usage: &Usage) -> u64 {
    [
        usage.input,
        usage.output,
        usage.cache_read,
        usage.cache_write,
    ]
    .into_iter()
    .flatten()
    .fold(0_u64, u64::saturating_add)
}

fn estimate_message_tokens(message: &Message) -> u64 {
    match message {
        Message::User(message) => message.content.iter().fold(0, |total, part| {
            total.saturating_add(match part {
                llm_contracts::ContentPart::Text(text) => estimate_text_tokens(&text.content),
                llm_contracts::ContentPart::Image(_) => ESTIMATED_IMAGE_TOKENS,
            })
        }),
        Message::System(message) => message.content.iter().fold(0, |total, part| {
            total.saturating_add(estimate_text_tokens(&part.content))
        }),
        Message::ToolResult(message) => message.content.iter().fold(0, |total, part| {
            total.saturating_add(match part {
                llm_contracts::ContentPart::Text(text) => estimate_text_tokens(&text.content),
                llm_contracts::ContentPart::Image(_) => ESTIMATED_IMAGE_TOKENS,
            })
        }),
        Message::Assistant(message) => estimate_json_tokens(&message.native_message),
        Message::Custom(message) => estimate_json_tokens(&Value::Object(message.content.clone())),
    }
}

fn estimate_json_tokens(value: &Value) -> u64 {
    serde_json::to_string(value).map_or(u64::MAX, |value| estimate_text_tokens(&value))
}

fn estimate_text_tokens(value: &str) -> u64 {
    u64::try_from(value.chars().count())
        .unwrap_or(u64::MAX)
        .saturating_add(3)
        / 4
}

#[derive(Debug, thiserror::Error)]
pub enum CompactionError {
    #[error("could not form the active request for Codex compaction")]
    Context(#[from] super::ContextFormationError),
    #[error("formed Codex compaction request is invalid")]
    InvalidRequest(#[source] ValidationError),
    #[error("compaction response provider mismatch: expected {expected:?}, got {actual:?}")]
    ResponseProviderMismatch { expected: String, actual: String },
    #[error("compaction response native message does not contain an output array")]
    MissingResponseOutput,
    #[error("compaction response contained {0} compaction items; expected exactly one")]
    UnexpectedCompactionItemCount(usize),
    #[error("compaction response item does not contain opaque encrypted_content")]
    MissingEncryptedContent,
    #[error("could not serialize Codex compaction checkpoint")]
    SerializeCheckpoint(#[source] serde_json::Error),
}

#[derive(Debug, thiserror::Error)]
pub enum ContextNormalizationError {
    #[error("session history contains message for {actual}, expected {expected}")]
    SessionMismatch { expected: Uuid, actual: Uuid },
    #[error("session history contains duplicate revision {0}")]
    DuplicateRevision(u64),
    #[error("Codex compaction message is invalid")]
    InvalidCompactionMessage(#[source] serde_json::Error),
    #[error("unsupported Codex compaction schema version {0}")]
    UnsupportedVersion(u32),
    #[error(
        "compaction checkpoint provider {checkpoint} does not match request provider {request}"
    )]
    ProviderMismatch {
        checkpoint: CodexProvider,
        request: CodexProvider,
    },
    #[error("compaction checkpoint must contain exactly one native compaction item")]
    InvalidCompactionItems,
    #[error("compaction checkpoint retained history may contain only user/developer messages")]
    InvalidRetainedMessage,
}

#[cfg(test)]
mod tests {
    use agent_contracts::{SessionMessage, SessionMessageDelivery, SessionMessageOrigin};
    use chrono::Utc;
    use llm_contracts::{
        AssistantContent, AssistantMessage, ContentPart, Message, MessageId, ModelId, ModelRef,
        ProviderId, StopReason, TextContent, Timestamp, ToolArguments, ToolCallId,
        ToolResultOutcome, Usage, UserMessage,
    };
    use serde_json::json;
    use uuid::Uuid;

    use super::{
        CODEX_COMPACTION_MESSAGE_TAG, CodexCompactionTrigger, CompactionPlan,
        checkpoint_from_response, form_compaction_request, normalize_session_messages,
        plan_compaction,
    };
    use crate::runtime::{CodexEnvironmentSnapshot, CodexHarnessConfig, CodexProvider};

    #[test]
    fn request_ends_with_native_trigger_for_both_providers() {
        let session_id = Uuid::now_v7();
        for provider in ["openai", "chatgpt"] {
            let request = form_compaction_request(
                &config(provider),
                session_id,
                &environment(),
                &[session_message(session_id, 1, user("user-1", "Fix it."))],
            )
            .expect("compaction request");
            let body = if provider == "openai" {
                provider_openai::build_response_request(&request).expect("OpenAI body")
            } else {
                provider_chatgpt::build_response_request(&request).expect("ChatGPT body")
            };
            assert_eq!(
                body["input"].as_array().expect("input").last(),
                Some(&json!({"type": "compaction_trigger"}))
            );
        }
    }

    #[test]
    fn checkpoint_replays_opaque_item_as_replacement_history() {
        let session_id = Uuid::now_v7();
        let active = vec![
            user("old-user", "Old request"),
            assistant("old-assistant", "Prior answer"),
            user("new-user", "Current request"),
        ];
        let checkpoint = checkpoint_from_response(
            &config("openai"),
            message_id("checkpoint"),
            Timestamp(10),
            CodexCompactionTrigger::AutomaticLimit,
            &active,
            &compaction_response("openai"),
        )
        .expect("checkpoint");
        let messages = vec![
            session_message(session_id, 1, active[0].clone()),
            session_message(session_id, 2, active[1].clone()),
            session_message(session_id, 3, active[2].clone()),
            session_message(session_id, 4, Message::Custom(checkpoint)),
            session_message(session_id, 5, user("after", "After compaction")),
        ];
        let normalized = normalize_session_messages(session_id, &messages, CodexProvider::OpenAi)
            .expect("normalized");
        assert_eq!(normalized.len(), 4);
        assert!(
            matches!(&normalized[0], Message::User(message) if message.id.as_str() == "old-user")
        );
        assert!(
            matches!(&normalized[1], Message::User(message) if message.id.as_str() == "new-user")
        );
        let Message::Custom(native) = &normalized[2] else {
            panic!("third item is native compaction")
        };
        assert_eq!(
            native.content["items"][0],
            json!({"type":"compaction","encrypted_content":"opaque"})
        );
        assert!(matches!(&normalized[3], Message::User(message) if message.id.as_str() == "after"));
    }

    #[test]
    fn interrupted_tool_call_gets_protocol_valid_error_before_next_user() {
        let session_id = Uuid::now_v7();
        let mut interrupted = compaction_response("chatgpt");
        interrupted.id = message_id("interrupted-assistant");
        interrupted.stop_reason = StopReason::ToolUse;
        interrupted.content = vec![AssistantContent::ToolCall {
            name: "exec".to_owned(),
            arguments: ToolArguments::String("text('never completed')".to_owned()),
            tool_call_id: ToolCallId::new("call-interrupted").expect("tool call ID"),
        }];
        let messages = vec![
            session_message(session_id, 1, Message::Assistant(interrupted)),
            session_message(session_id, 2, user("follow-up", "Continue")),
        ];

        let normalized = normalize_session_messages(session_id, &messages, CodexProvider::ChatGpt)
            .expect("normalized");
        assert_eq!(normalized.len(), 3);
        let Message::ToolResult(result) = &normalized[1] else {
            panic!("interrupted result must precede the next user message")
        };
        assert_eq!(result.tool_name, "exec");
        assert_eq!(result.tool_call_id.as_str(), "call-interrupted");
        assert!(matches!(result.outcome, ToolResultOutcome::Error { .. }));

        let request = llm_contracts::LlmRequest {
            model: crate::runtime::resolve_model_config(&config("chatgpt"), session_id).model,
            instructions: None,
            messages: normalized,
            tools: crate::runtime::model_visible_tool_definitions(),
            provider_options: Default::default(),
            metadata: Default::default(),
        };
        let body = provider_chatgpt::build_response_request(&request).expect("ChatGPT body");
        assert_eq!(body["input"][1]["type"], json!("custom_tool_call_output"));
        assert_eq!(body["input"][1]["call_id"], json!("call-interrupted"));
    }

    #[test]
    fn prior_overflow_compacts_once_at_the_next_sampling_boundary() {
        let session_id = Uuid::now_v7();
        let run_id = Uuid::now_v7();
        let request = form_compaction_request(&config("openai"), session_id, &environment(), &[])
            .expect("request");
        assert!(matches!(
            plan_compaction(&request, &[], 272_000, true, run_id, 2),
            CompactionPlan::Compact {
                trigger: CodexCompactionTrigger::ContextOverflow,
                token_limit: 244_800,
                ..
            }
        ));
        let mut checkpoint = session_message(
            session_id,
            1,
            Message::Custom(
                checkpoint_from_response(
                    &config("openai"),
                    message_id("checkpoint"),
                    Timestamp(1),
                    CodexCompactionTrigger::ContextOverflow,
                    &[],
                    &compaction_response("openai"),
                )
                .expect("checkpoint"),
            ),
        );
        checkpoint.run_id = Some(run_id);
        checkpoint.turn_number = Some(2);
        assert!(matches!(
            plan_compaction(&request, &[checkpoint], 272_000, true, run_id, 2),
            CompactionPlan::AlreadyCompactedForTurn { .. }
        ));
    }

    #[test]
    fn automatic_compaction_uses_last_server_token_usage() {
        let session_id = Uuid::now_v7();
        let run_id = Uuid::now_v7();
        let mut request =
            form_compaction_request(&config("openai"), session_id, &environment(), &[])
                .expect("request");
        let mut response = compaction_response("openai");
        response.usage = Some(Usage {
            input: Some(200_000),
            output: Some(4_800),
            cache_read: Some(40_000),
            cache_write: None,
            cost: None,
        });
        request.messages.push(Message::Assistant(response));
        assert!(matches!(
            plan_compaction(&request, &[], 272_000, false, run_id, 1),
            CompactionPlan::Compact {
                trigger: CodexCompactionTrigger::AutomaticLimit,
                estimated_tokens: 244_800,
                token_limit: 244_800,
            }
        ));
    }

    #[test]
    fn checkpoint_contains_no_plaintext_summary() {
        let checkpoint = checkpoint_from_response(
            &config("chatgpt"),
            message_id("checkpoint"),
            Timestamp(1),
            CodexCompactionTrigger::AutomaticLimit,
            &[user("user", "Keep me")],
            &compaction_response("chatgpt"),
        )
        .expect("checkpoint");
        assert_eq!(
            checkpoint.tag.as_deref(),
            Some(CODEX_COMPACTION_MESSAGE_TAG)
        );
        assert_eq!(checkpoint.content["provider"], json!("chatgpt"));
        assert_eq!(checkpoint.content["version"], json!(1));
        assert!(checkpoint.content.get("summary").is_none());
    }

    #[test]
    fn checkpoint_does_not_retain_ephemeral_environment_context() {
        let session_id = Uuid::now_v7();
        let environment = environment().as_message(session_id);
        let checkpoint = checkpoint_from_response(
            &config("openai"),
            message_id("checkpoint"),
            Timestamp(2),
            CodexCompactionTrigger::AutomaticLimit,
            &[environment, user("user", "Keep me")],
            &compaction_response("openai"),
        )
        .expect("checkpoint");
        let retained = checkpoint.content["retained_messages"]
            .as_array()
            .expect("retained messages");
        assert_eq!(retained.len(), 1);
        assert_eq!(retained[0]["id"], json!("user"));
    }

    fn config(provider: &str) -> CodexHarnessConfig {
        CodexHarnessConfig::from_resolved(
            json!({
                "provider": provider,
                "model_id": "gpt-5.6-sol",
                "reasoning_level": "low",
                "execution": {
                    "machine_id": "machine-a",
                    "workspace_root_id": "root",
                    "cwd": "."
                }
            })
            .as_object()
            .expect("object"),
        )
        .expect("config")
    }

    fn environment() -> CodexEnvironmentSnapshot {
        CodexEnvironmentSnapshot::new(
            "/workspace",
            Some("zsh".to_owned()),
            "2026-08-30",
            "Asia/Kolkata",
            vec!["/workspace".to_owned()],
            Timestamp(1),
        )
        .expect("environment")
    }

    fn compaction_response(provider: &str) -> AssistantMessage {
        AssistantMessage {
            id: message_id("response"),
            model: ModelRef {
                provider: ProviderId::new(provider).expect("provider"),
                id: ModelId::new("gpt-5.6-sol").expect("model"),
                name: None,
            },
            usage: None,
            duration_ms: 1,
            native_message: json!({
                "output": [{"type":"compaction","encrypted_content":"opaque"}]
            }),
            content: Vec::new(),
            stop_reason: StopReason::Stop,
            timestamp: Timestamp(1),
        }
    }

    fn session_message(session_id: Uuid, revision: u64, message: Message) -> SessionMessage {
        SessionMessage {
            session_message_id: Uuid::now_v7(),
            session_id,
            revision,
            message,
            origin: SessionMessageOrigin::Harness,
            delivery: SessionMessageDelivery::Immediate,
            run_id: None,
            turn_number: None,
            created_at: Utc::now(),
            committed_at: Utc::now(),
        }
    }

    fn user(id: &str, content: &str) -> Message {
        Message::User(UserMessage {
            id: message_id(id),
            timestamp: Timestamp(1),
            content: vec![ContentPart::Text(TextContent {
                content: content.to_owned(),
                metadata: None,
            })],
        })
    }

    fn assistant(id: &str, content: &str) -> Message {
        let mut response = compaction_response("openai");
        response.id = message_id(id);
        response.native_message = json!({
            "output": [{
                "type": "message",
                "role": "assistant",
                "content": [{"type":"output_text", "text": content}]
            }]
        });
        Message::Assistant(response)
    }

    fn message_id(value: &str) -> MessageId {
        MessageId::new(value).expect("message ID")
    }
}
