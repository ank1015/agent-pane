use std::collections::HashSet;

use agent_contracts::SessionMessage;
use llm_contracts::{
    AssistantContent, AssistantMessage, ContentPart, CustomMessage, ImageDetail, ImageSource,
    LlmError, LlmRequest, Message, MessageId, TextContent, Timestamp, ToolResultMessage,
    ToolResultOutcome, Usage, Validate, ValidationError,
};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use uuid::Uuid;

use super::{CodexHarnessConfig, CodexProvider, form_main_request};

pub const CODEX_COMPACTION_MESSAGE_TAG: &str = "codex.compaction";
pub const CODEX_COMPACTION_SCHEMA_VERSION: u32 = 1;
pub const RETAINED_MESSAGE_TOKEN_BUDGET: u64 = 64_000;

const APPROX_BYTES_PER_TOKEN: u64 = 4;
const RESIZED_IMAGE_BYTES_ESTIMATE: u64 = 7_373;
const EFFECTIVE_CONTEXT_WINDOW_PERCENT: u64 = 95;
const CONTEXT_WINDOW_TRUNCATED_OUTPUT_MESSAGE: &str =
    "Output exceeded the available model context and was truncated";
// Must match Codex so prompt-only missing outputs keep stable item IDs across
// retries and resume.
const SYNTHETIC_OUTPUT_ID_NAMESPACE: Uuid = Uuid::from_u128(0x90d38d3e_6a5b_4d52_bfe2_2f1e634bfac4);

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
    /// Messages delivered for the current turn after the history snapshot that
    /// was sent to the compaction endpoint. They are installed after the opaque
    /// compaction item and are deliberately excluded from the 64k retention
    /// budget and from the compaction request itself.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub pending_messages: Vec<Message>,
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
    session_messages: &[SessionMessage],
) -> Result<LlmRequest, CompactionError> {
    let mut request = form_main_request(config, session_id, session_messages)?;
    trim_tool_outputs_for_compaction(&mut request, config.model.profile().context_window);
    request.messages.push(Message::Custom(CustomMessage {
        id: MessageId::new(format!("codex-compaction-trigger-{session_id}"))
            .expect("UUID-derived message ID is valid"),
        content: json!({ "items": [{ "type": "compaction_trigger" }] })
            .as_object()
            .expect("static value is an object")
            .clone(),
        tag: Some(native_input_tag(config.provider).to_owned()),
        timestamp: latest_message_timestamp(&request.messages).unwrap_or(Timestamp(0)),
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
    compacted_messages: &[Message],
    pending_messages: &[Message],
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

    let retained_messages = select_retained_messages(compacted_messages);
    let active_context_tokens = retained_messages
        .iter()
        .map(estimate_message_tokens)
        .sum::<u64>()
        .saturating_add(estimate_native_item_tokens(&compaction_items[0]))
        .saturating_add(
            pending_messages
                .iter()
                .map(estimate_message_tokens)
                .fold(0_u64, u64::saturating_add),
        );
    create_codex_compaction_message(
        id,
        timestamp,
        CodexCompactionMessageContent {
            version: CODEX_COMPACTION_SCHEMA_VERSION,
            provider: config.provider,
            trigger,
            retained_messages,
            pending_messages: pending_messages.to_vec(),
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
    if has_compacted_for_turn(session_messages, run_id, turn_number) {
        return CompactionPlan::AlreadyCompactedForTurn {
            estimated_tokens,
            token_limit,
        };
    }
    if prior_context_overflow {
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
        normalized.extend(checkpoint.pending_messages);
        extend_visible(&mut normalized, &messages[checkpoint_index + 1..], provider);
    } else {
        extend_visible(&mut normalized, &messages, provider);
    }
    repair_interrupted_tool_calls(&mut normalized);
    Ok(normalized)
}

/// Enforces the Responses call/output invariants on portable history. Missing
/// outputs are inserted immediately after their assistant call message using
/// Codex's raw `aborted` payload, and outputs without a call anywhere in active
/// history are removed.
fn repair_interrupted_tool_calls(messages: &mut Vec<Message>) {
    let call_ids = messages
        .iter()
        .filter_map(|message| match message {
            Message::Assistant(assistant) => Some(assistant.content.iter()),
            _ => None,
        })
        .flatten()
        .filter_map(|content| match content {
            AssistantContent::ToolCall { tool_call_id, .. } => Some(tool_call_id.clone()),
            _ => None,
        })
        .collect::<HashSet<_>>();

    messages.retain(|message| {
        !matches!(
            message,
            Message::ToolResult(result) if !call_ids.contains(&result.tool_call_id)
        )
    });

    let output_ids = messages
        .iter()
        .filter_map(|message| match message {
            Message::ToolResult(result) => Some(result.tool_call_id.clone()),
            _ => None,
        })
        .collect::<HashSet<_>>();
    let mut repaired = Vec::with_capacity(messages.len());
    for message in messages.drain(..) {
        let Message::Assistant(mut assistant) = message else {
            repaired.push(message);
            continue;
        };
        let mut missing = assistant
            .content
            .iter()
            .filter_map(|content| match content {
                AssistantContent::ToolCall {
                    name, tool_call_id, ..
                } if !output_ids.contains(tool_call_id) => {
                    Some((name.clone(), tool_call_id.clone(), assistant.timestamp))
                }
                _ => None,
            })
            .collect::<Vec<_>>();
        let missing_ids = missing
            .iter()
            .map(|(_, tool_call_id, _)| tool_call_id.clone())
            .collect::<HashSet<_>>();
        let inserted = inject_native_aborted_outputs(&mut assistant, &missing_ids);
        missing.retain(|(_, tool_call_id, _)| !inserted.contains(tool_call_id));
        repaired.push(Message::Assistant(assistant));
        repaired.extend(
            missing
                .into_iter()
                .map(|(tool_name, tool_call_id, timestamp)| {
                    Message::ToolResult(ToolResultMessage {
                        id: MessageId::new(format!("codex-interrupted-tool-{tool_call_id}"))
                            .expect("tool-call-derived message ID is valid"),
                        tool_name,
                        tool_call_id,
                        content: vec![ContentPart::Text(TextContent {
                            content: "aborted".to_owned(),
                            metadata: None,
                        })],
                        details: None,
                        timestamp,
                        // Responses does not serialize this portable status. Mark
                        // it successful so providers that decorate failures still
                        // emit exactly the raw Codex `aborted` payload.
                        outcome: ToolResultOutcome::Success,
                    })
                }),
        );
    }
    *messages = repaired;
}

/// Same-provider assistant messages replay their native Responses items. Put a
/// synthetic output directly after its call in that array, matching Codex's
/// item-level normalization. The portable fallback below is only needed for
/// imported/non-native assistant messages whose raw call item is unavailable.
fn inject_native_aborted_outputs(
    assistant: &mut AssistantMessage,
    missing: &HashSet<llm_contracts::ToolCallId>,
) -> HashSet<llm_contracts::ToolCallId> {
    if missing.is_empty() {
        return HashSet::new();
    }
    let Some(output) = assistant
        .native_message
        .get_mut("output")
        .and_then(Value::as_array_mut)
    else {
        return HashSet::new();
    };

    let mut inserted = HashSet::new();
    let original = std::mem::take(output);
    output.reserve(original.len().saturating_add(missing.len()));
    for item in original {
        let synthetic = native_aborted_output(&item, missing);
        output.push(item);
        if let Some((tool_call_id, synthetic)) = synthetic {
            inserted.insert(tool_call_id);
            output.push(synthetic);
        }
    }
    inserted
}

fn native_aborted_output(
    call: &Value,
    missing: &HashSet<llm_contracts::ToolCallId>,
) -> Option<(llm_contracts::ToolCallId, Value)> {
    let kind = call.get("type").and_then(Value::as_str)?;
    let (output_kind, id_prefix) = match kind {
        "function_call" | "local_shell_call" => ("function_call_output", "fco"),
        "custom_tool_call" => ("custom_tool_call_output", "ctco"),
        _ => return None,
    };
    let tool_call_id =
        llm_contracts::ToolCallId::new(call.get("call_id").and_then(Value::as_str)?.to_owned())
            .ok()?;
    if !missing.contains(&tool_call_id) {
        return None;
    }
    let mut synthetic = serde_json::Map::from_iter([
        ("type".to_owned(), Value::String(output_kind.to_owned())),
        (
            "call_id".to_owned(),
            Value::String(tool_call_id.as_str().to_owned()),
        ),
        ("output".to_owned(), Value::String("aborted".to_owned())),
    ]);
    if let Some(source_id) = call
        .get("id")
        .and_then(Value::as_str)
        .filter(|id| !id.is_empty())
    {
        let name = format!("{id_prefix}:{source_id}");
        let suffix = Uuid::new_v5(&SYNTHETIC_OUTPUT_ID_NAMESPACE, name.as_bytes());
        synthetic.insert(
            "id".to_owned(),
            Value::String(format!("{id_prefix}_{suffix}")),
        );
    }
    Some((tool_call_id, Value::Object(synthetic)))
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
        .any(|message| !matches!(message, Message::User(_)))
    {
        return Err(ContextNormalizationError::InvalidRetainedMessage);
    }
    if checkpoint.pending_messages.iter().any(|message| {
        matches!(
            message,
            Message::Custom(custom)
                if custom.tag.as_deref() == Some(CODEX_COMPACTION_MESSAGE_TAG)
        )
    }) {
        return Err(ContextNormalizationError::InvalidPendingMessage);
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
        let Message::User(user) = message else {
            continue;
        };
        if is_ephemeral_environment(message) {
            continue;
        }
        let tokens = retained_user_text_tokens(user).max(1);
        if tokens <= remaining {
            retained.push(message.clone());
            remaining = remaining.saturating_sub(tokens);
        } else if remaining > 0
            && let Some(truncated) = truncate_retained_user(user, remaining)
        {
            retained.push(Message::User(truncated));
            remaining = 0;
        }
        if remaining == 0 {
            break;
        }
    }
    retained.reverse();
    retained
}

fn retained_user_text_tokens(message: &llm_contracts::UserMessage) -> u64 {
    message
        .content
        .iter()
        .filter_map(|part| match part {
            ContentPart::Text(text) => Some(estimate_text_tokens(&text.content)),
            ContentPart::Image(_) => None,
        })
        .fold(0_u64, u64::saturating_add)
}

fn truncate_retained_user(
    message: &llm_contracts::UserMessage,
    max_tokens: u64,
) -> Option<llm_contracts::UserMessage> {
    let mut remaining = max_tokens;
    let mut content = Vec::with_capacity(message.content.len());
    for part in &message.content {
        match part {
            ContentPart::Text(text) => {
                if remaining == 0 {
                    continue;
                }
                let tokens = estimate_text_tokens(&text.content);
                if tokens <= remaining {
                    content.push(part.clone());
                    remaining = remaining.saturating_sub(tokens);
                } else {
                    let truncated = truncate_text_to_token_budget(&text.content, remaining);
                    if !truncated.is_empty() {
                        content.push(ContentPart::Text(TextContent {
                            content: truncated,
                            metadata: text.metadata.clone(),
                        }));
                    }
                    remaining = 0;
                }
            }
            // Images are retained in full and do not consume the remote
            // compaction text-retention budget.
            ContentPart::Image(_) => content.push(part.clone()),
        }
    }
    (!content.is_empty()).then(|| llm_contracts::UserMessage {
        id: message.id.clone(),
        timestamp: message.timestamp,
        content,
    })
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

/// Rewrites only the newest contiguous tool-result messages until the
/// compaction input fits Codex's effective (95%) model context window. The
/// durable transcript is not mutated; this is a prompt-only safety pass.
pub(crate) fn trim_tool_outputs_for_compaction(
    request: &mut LlmRequest,
    model_context_window: u64,
) -> usize {
    let hard_limit = model_context_window.saturating_mul(EFFECTIVE_CONTEXT_WINDOW_PERCENT) / 100;
    let mut estimated_tokens = estimate_full_history_tokens(request);
    let mut rewritten = 0;

    for index in (0..request.messages.len()).rev() {
        if estimated_tokens <= hard_limit {
            break;
        }
        let Message::ToolResult(result) = &request.messages[index] else {
            // Codex only rewrites the newest contiguous output groups. Once a
            // non-output group is encountered, older outputs are not touched.
            break;
        };
        let replacement = Message::ToolResult(ToolResultMessage {
            id: result.id.clone(),
            tool_name: result.tool_name.clone(),
            tool_call_id: result.tool_call_id.clone(),
            content: vec![ContentPart::Text(TextContent {
                content: CONTEXT_WINDOW_TRUNCATED_OUTPUT_MESSAGE.to_owned(),
                metadata: None,
            })],
            details: None,
            timestamp: result.timestamp,
            outcome: result.outcome.clone(),
        });
        let old_tokens = estimate_message_tokens(&request.messages[index]);
        let new_tokens = estimate_message_tokens(&replacement);
        request.messages[index] = replacement;
        estimated_tokens = estimated_tokens
            .saturating_sub(old_tokens)
            .saturating_add(new_tokens);
        rewritten += 1;
    }

    rewritten
}

fn estimate_full_history_tokens(request: &LlmRequest) -> u64 {
    let instructions = request
        .instructions
        .as_deref()
        .map_or(0, estimate_text_tokens);
    request
        .messages
        .iter()
        .map(estimate_message_tokens)
        .fold(instructions, u64::saturating_add)
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
    wire_items_for_message(message)
        .iter()
        .map(estimate_native_item_tokens)
        .fold(0_u64, u64::saturating_add)
}

fn estimate_json_tokens(value: &Value) -> u64 {
    estimate_native_item_tokens(value)
}

fn estimate_text_tokens(value: &str) -> u64 {
    tokens_from_bytes(u64::try_from(value.len()).unwrap_or(u64::MAX))
}

fn tokens_from_bytes(bytes: u64) -> u64 {
    bytes.saturating_add(APPROX_BYTES_PER_TOKEN - 1) / APPROX_BYTES_PER_TOKEN
}

fn wire_items_for_message(message: &Message) -> Vec<Value> {
    match message {
        Message::User(message) => vec![json!({
            "type": "message",
            "role": "user",
            "content": wire_content_parts(&message.content),
        })],
        Message::System(message) => vec![json!({
            "type": "message",
            "role": "developer",
            "content": message.content.iter().map(|part| {
                json!({"type": "input_text", "text": part.content})
            }).collect::<Vec<_>>(),
        })],
        Message::ToolResult(message) => {
            let output = if message
                .content
                .iter()
                .all(|part| matches!(part, ContentPart::Text(_)))
            {
                Value::String(
                    message
                        .content
                        .iter()
                        .filter_map(|part| match part {
                            ContentPart::Text(text) => Some(text.content.as_str()),
                            ContentPart::Image(_) => None,
                        })
                        .collect::<Vec<_>>()
                        .join("\n"),
                )
            } else {
                Value::Array(wire_content_parts(&message.content))
            };
            vec![json!({
                "type": "custom_tool_call_output",
                "call_id": message.tool_call_id,
                "output": output,
            })]
        }
        Message::Assistant(message) => message
            .native_message
            .get("output")
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_else(|| portable_assistant_items(message)),
        Message::Custom(message) => message
            .content
            .get("items")
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_else(|| vec![Value::Object(message.content.clone())]),
    }
}

fn portable_assistant_items(message: &AssistantMessage) -> Vec<Value> {
    message
        .content
        .iter()
        .filter_map(|part| match part {
            AssistantContent::Response { response } if !response.content.is_empty() => {
                Some(json!({
                    "type": "message",
                    "role": "assistant",
                    "content": response.content,
                }))
            }
            AssistantContent::ToolCall {
                name,
                arguments,
                tool_call_id,
            } => {
                let input = match arguments {
                    llm_contracts::ToolArguments::Object(arguments) => {
                        serde_json::to_string(arguments).unwrap_or_default()
                    }
                    llm_contracts::ToolArguments::String(arguments) => arguments.clone(),
                };
                Some(json!({
                    "type": "custom_tool_call",
                    "call_id": tool_call_id,
                    "name": name,
                    "input": input,
                }))
            }
            AssistantContent::Response { .. } | AssistantContent::Thinking { .. } => None,
        })
        .collect()
}

fn wire_content_parts(content: &[ContentPart]) -> Vec<Value> {
    content
        .iter()
        .map(|part| match part {
            ContentPart::Text(text) => json!({"type": "input_text", "text": text.content}),
            ContentPart::Image(image) => {
                let image_url = match &image.source {
                    ImageSource::Base64(source) => {
                        format!("data:{};base64,{}", source.mime_type, source.data)
                    }
                    ImageSource::Url(source) => source.url.clone(),
                };
                let detail = image.detail.map_or("auto", image_detail_name);
                json!({
                    "type": "input_image",
                    "detail": detail,
                    "image_url": image_url,
                })
            }
        })
        .collect()
}

const fn image_detail_name(detail: ImageDetail) -> &'static str {
    match detail {
        ImageDetail::Auto => "auto",
        ImageDetail::Low => "low",
        ImageDetail::High => "high",
        ImageDetail::Original => "original",
    }
}

fn estimate_native_item_tokens(item: &Value) -> u64 {
    let kind = item.get("type").and_then(Value::as_str);
    if matches!(
        kind,
        Some("reasoning" | "compaction" | "compaction_summary" | "context_compaction")
    ) && let Some(content) = item.get("encrypted_content").and_then(Value::as_str)
    {
        let visible_bytes = u64::try_from(content.len())
            .unwrap_or(u64::MAX)
            .saturating_mul(3)
            / 4;
        return tokens_from_bytes(visible_bytes.saturating_sub(650));
    }

    let raw_bytes = serde_json::to_vec(item)
        .map(|serialized| u64::try_from(serialized.len()).unwrap_or(u64::MAX))
        .unwrap_or_default();
    let mut removed_bytes = 0_u64;
    let mut replacement_bytes = 0_u64;
    collect_native_payload_adjustments(item, &mut removed_bytes, &mut replacement_bytes);
    tokens_from_bytes(
        raw_bytes
            .saturating_sub(removed_bytes)
            .saturating_add(replacement_bytes),
    )
}

fn collect_native_payload_adjustments(
    value: &Value,
    removed_bytes: &mut u64,
    replacement_bytes: &mut u64,
) {
    match value {
        Value::Array(values) => {
            for value in values {
                collect_native_payload_adjustments(value, removed_bytes, replacement_bytes);
            }
        }
        Value::Object(object) => {
            if object.get("type").and_then(Value::as_str) == Some("encrypted_content")
                && let Some(content) = object.get("encrypted_content").and_then(Value::as_str)
            {
                let encoded_len = u64::try_from(content.len()).unwrap_or(u64::MAX);
                *removed_bytes = removed_bytes.saturating_add(encoded_len);
                *replacement_bytes =
                    replacement_bytes.saturating_add(encoded_len.saturating_mul(9).div_ceil(16));
            }
            if let Some(image_url) = object.get("image_url").and_then(Value::as_str)
                && let Some(payload) = base64_data_url_payload(image_url, "image/")
            {
                *removed_bytes =
                    removed_bytes.saturating_add(u64::try_from(payload.len()).unwrap_or(u64::MAX));
                *replacement_bytes = replacement_bytes.saturating_add(RESIZED_IMAGE_BYTES_ESTIMATE);
            }
            for value in object.values() {
                collect_native_payload_adjustments(value, removed_bytes, replacement_bytes);
            }
        }
        Value::Null | Value::Bool(_) | Value::Number(_) | Value::String(_) => {}
    }
}

fn base64_data_url_payload<'a>(url: &'a str, media_type_prefix: &str) -> Option<&'a str> {
    if !url
        .get(.."data:".len())
        .is_some_and(|prefix| prefix.eq_ignore_ascii_case("data:"))
    {
        return None;
    }
    let comma = url.find(',')?;
    let metadata = &url["data:".len()..comma];
    let mut parts = metadata.split(';');
    let mime_type = parts.next().unwrap_or_default();
    if !mime_type
        .get(..media_type_prefix.len())
        .is_some_and(|prefix| prefix.eq_ignore_ascii_case(media_type_prefix))
        || !parts.any(|part| part.eq_ignore_ascii_case("base64"))
    {
        return None;
    }
    Some(&url[comma + 1..])
}

fn truncate_text_to_token_budget(value: &str, max_tokens: u64) -> String {
    if value.is_empty() {
        return String::new();
    }
    let max_bytes =
        usize::try_from(max_tokens.saturating_mul(APPROX_BYTES_PER_TOKEN)).unwrap_or(usize::MAX);
    if max_tokens > 0 && value.len() <= max_bytes {
        return value.to_owned();
    }

    let left_budget = max_bytes / 2;
    let right_budget = max_bytes.saturating_sub(left_budget);
    let tail_start = value.len().saturating_sub(right_budget);
    let mut prefix_end = 0;
    let mut suffix_start = value.len();
    let mut suffix_started = false;
    for (index, character) in value.char_indices() {
        let character_end = index.saturating_add(character.len_utf8());
        if character_end <= left_budget {
            prefix_end = character_end;
        } else if index >= tail_start && !suffix_started {
            suffix_start = index;
            suffix_started = true;
        }
    }
    if suffix_start < prefix_end {
        suffix_start = prefix_end;
    }
    let removed_bytes = value
        .len()
        .saturating_sub(prefix_end)
        .saturating_sub(value.len().saturating_sub(suffix_start));
    format!(
        "{}…{} tokens truncated…{}",
        &value[..prefix_end],
        tokens_from_bytes(u64::try_from(removed_bytes).unwrap_or(u64::MAX)),
        &value[suffix_start..]
    )
}

fn latest_message_timestamp(messages: &[Message]) -> Option<Timestamp> {
    messages.last().map(message_timestamp)
}

fn message_timestamp(message: &Message) -> Timestamp {
    match message {
        Message::User(message) => message.timestamp,
        Message::System(message) => message.timestamp,
        Message::ToolResult(message) => message.timestamp,
        Message::Assistant(message) => message.timestamp,
        Message::Custom(message) => message.timestamp,
    }
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
    #[error("compaction checkpoint retained history may contain only real user messages")]
    InvalidRetainedMessage,
    #[error("compaction checkpoint pending history may not contain another compaction checkpoint")]
    InvalidPendingMessage,
}

#[cfg(test)]
mod tests {
    use agent_contracts::{SessionMessage, SessionMessageDelivery, SessionMessageOrigin};
    use chrono::Utc;
    use llm_contracts::{
        AssistantContent, AssistantMessage, ContentPart, ImageContent, ImageDetail, ImageSource,
        Message, MessageId, ModelId, ModelRef, ProviderId, StopReason, SystemMessage, TextContent,
        Timestamp, ToolArguments, ToolCallId, ToolResultMessage, ToolResultOutcome, UrlImageSource,
        Usage, UserMessage,
    };
    use serde_json::json;
    use uuid::Uuid;

    use super::{
        CODEX_COMPACTION_MESSAGE_TAG, CodexCompactionTrigger, CompactionPlan,
        checkpoint_from_response, estimate_native_item_tokens, estimate_text_tokens,
        form_compaction_request, normalize_session_messages, plan_compaction,
    };
    use crate::runtime::{CodexEnvironmentSnapshot, CodexHarnessConfig, CodexProvider};

    #[test]
    fn request_ends_with_native_trigger_for_both_providers() {
        let session_id = Uuid::now_v7();
        for provider in ["openai", "chatgpt"] {
            let request = form_compaction_request(
                &config(provider),
                session_id,
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
            &[],
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
    fn interrupted_tool_call_gets_raw_aborted_output_before_next_user() {
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
        assert!(matches!(result.outcome, ToolResultOutcome::Success));
        assert_eq!(
            result.content,
            vec![ContentPart::Text(TextContent {
                content: "aborted".to_owned(),
                metadata: None,
            })]
        );

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
        assert_eq!(body["input"][1]["output"], json!("aborted"));
    }

    #[test]
    fn native_missing_output_is_inserted_immediately_after_its_call() {
        let session_id = Uuid::now_v7();
        let mut interrupted = compaction_response("openai");
        interrupted.id = message_id("parallel-assistant");
        interrupted.stop_reason = StopReason::ToolUse;
        interrupted.content = vec![
            AssistantContent::ToolCall {
                name: "exec".to_owned(),
                arguments: ToolArguments::String("text('first')".to_owned()),
                tool_call_id: ToolCallId::new("call-1").expect("tool call ID"),
            },
            AssistantContent::ToolCall {
                name: "exec".to_owned(),
                arguments: ToolArguments::String("text('second')".to_owned()),
                tool_call_id: ToolCallId::new("call-2").expect("tool call ID"),
            },
        ];
        interrupted.native_message = json!({
            "output": [
                {
                    "type": "custom_tool_call",
                    "id": "ctc-server-1",
                    "call_id": "call-1",
                    "name": "exec",
                    "input": "text('first')"
                },
                {
                    "type": "custom_tool_call",
                    "id": "ctc-server-2",
                    "call_id": "call-2",
                    "name": "exec",
                    "input": "text('second')"
                }
            ]
        });
        let messages = vec![
            session_message(session_id, 1, Message::Assistant(interrupted)),
            session_message(
                session_id,
                2,
                tool_result("result-2", "call-2", "completed"),
            ),
            session_message(session_id, 3, user("follow-up", "Continue")),
        ];

        let normalized = normalize_session_messages(session_id, &messages, CodexProvider::OpenAi)
            .expect("normalized");
        assert_eq!(
            normalized.len(),
            3,
            "synthetic output stays in native items"
        );
        let Message::Assistant(assistant) = &normalized[0] else {
            panic!("first message is the assistant")
        };
        let output = assistant.native_message["output"]
            .as_array()
            .expect("native output");
        assert_eq!(output[0]["call_id"], json!("call-1"));
        assert_eq!(output[1]["type"], json!("custom_tool_call_output"));
        assert_eq!(output[1]["call_id"], json!("call-1"));
        assert_eq!(output[1]["output"], json!("aborted"));
        assert!(
            output[1]["id"]
                .as_str()
                .is_some_and(|id| id.starts_with("ctco_")),
            "Codex-compatible synthetic IDs remain stable for prompt caching"
        );
        assert_eq!(output[2]["call_id"], json!("call-2"));

        let request = llm_contracts::LlmRequest {
            model: crate::runtime::resolve_model_config(&config("openai"), session_id).model,
            instructions: None,
            messages: normalized,
            tools: crate::runtime::model_visible_tool_definitions(),
            provider_options: Default::default(),
            metadata: Default::default(),
        };
        let body = provider_openai::build_response_request(&request).expect("OpenAI body");
        let input = body["input"].as_array().expect("input");
        assert_eq!(input[0]["call_id"], json!("call-1"));
        assert_eq!(input[1]["type"], json!("custom_tool_call_output"));
        assert_eq!(input[2]["call_id"], json!("call-2"));
        assert_eq!(input[3]["call_id"], json!("call-2"));
        assert_eq!(input[3]["output"], json!("completed"));
    }

    #[test]
    fn normalization_removes_orphan_tool_outputs() {
        let session_id = Uuid::now_v7();
        let messages = vec![
            session_message(
                session_id,
                1,
                tool_result("orphan-result", "call-orphan", "unmatched output"),
            ),
            session_message(session_id, 2, user("follow-up", "Continue")),
        ];

        let normalized = normalize_session_messages(session_id, &messages, CodexProvider::OpenAi)
            .expect("normalized");
        assert_eq!(normalized, vec![user("follow-up", "Continue")]);
    }

    #[test]
    fn pending_user_is_installed_after_opaque_compaction_item() {
        let session_id = Uuid::now_v7();
        let old = user("old-user", "Old request");
        let pending = user("pending-user", "Current request");
        let checkpoint = checkpoint_from_response(
            &config("openai"),
            message_id("checkpoint"),
            Timestamp(10),
            CodexCompactionTrigger::AutomaticLimit,
            std::slice::from_ref(&old),
            std::slice::from_ref(&pending),
            &compaction_response("openai"),
        )
        .expect("checkpoint");
        let messages = vec![
            session_message(session_id, 1, old),
            session_message(session_id, 2, pending),
            session_message(session_id, 3, Message::Custom(checkpoint)),
        ];

        let normalized = normalize_session_messages(session_id, &messages, CodexProvider::OpenAi)
            .expect("normalized");
        assert_eq!(normalized.len(), 3);
        assert!(
            matches!(&normalized[0], Message::User(message) if message.id.as_str() == "old-user")
        );
        assert!(matches!(&normalized[1], Message::Custom(_)));
        assert!(
            matches!(&normalized[2], Message::User(message) if message.id.as_str() == "pending-user")
        );
    }

    #[test]
    fn prior_overflow_compacts_once_at_the_next_sampling_boundary() {
        let session_id = Uuid::now_v7();
        let run_id = Uuid::now_v7();
        let request = form_compaction_request(&config("openai"), session_id, &[]).expect("request");
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
            form_compaction_request(&config("openai"), session_id, &[]).expect("request");
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
            &[],
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
            &[],
            &compaction_response("openai"),
        )
        .expect("checkpoint");
        let retained = checkpoint.content["retained_messages"]
            .as_array()
            .expect("retained messages");
        assert_eq!(retained.len(), 1);
        assert_eq!(retained[0]["id"], json!("user"));
    }

    #[test]
    fn checkpoint_retains_only_real_users_and_truncates_newest_boundary() {
        let huge = "x".repeat(300_000);
        let checkpoint = checkpoint_from_response(
            &config("openai"),
            message_id("checkpoint"),
            Timestamp(2),
            CodexCompactionTrigger::AutomaticLimit,
            &[
                user(
                    "older-user",
                    "This older message must not leapfrog the boundary",
                ),
                system("developer", "Drop developer context"),
                user("newest-user", &huge),
            ],
            &[],
            &compaction_response("openai"),
        )
        .expect("checkpoint");
        let retained = checkpoint.content["retained_messages"]
            .as_array()
            .expect("retained messages");
        assert_eq!(retained.len(), 1);
        assert_eq!(retained[0]["id"], json!("newest-user"));
        let retained_text = retained[0]["content"][0]["content"]
            .as_str()
            .expect("retained text");
        assert!(retained_text.contains("tokens truncated"));
        assert!(retained_text.len() < huge.len());
    }

    #[test]
    fn checkpoint_preserves_url_images_without_charging_the_text_budget() {
        let huge = "x".repeat(300_000);
        let image = ContentPart::Image(ImageContent {
            source: ImageSource::Url(UrlImageSource {
                url: "https://example.com/input.png".to_owned(),
            }),
            detail: Some(ImageDetail::High),
            metadata: None,
        });
        let mixed = Message::User(UserMessage {
            id: message_id("mixed-user"),
            timestamp: Timestamp(1),
            content: vec![
                ContentPart::Text(TextContent {
                    content: huge,
                    metadata: None,
                }),
                image.clone(),
            ],
        });
        let checkpoint = checkpoint_from_response(
            &config("openai"),
            message_id("checkpoint"),
            Timestamp(2),
            CodexCompactionTrigger::AutomaticLimit,
            &[mixed],
            &[],
            &compaction_response("openai"),
        )
        .expect("checkpoint");
        let retained = checkpoint.content["retained_messages"]
            .as_array()
            .expect("retained messages");
        let retained_message: Message =
            serde_json::from_value(retained[0].clone()).expect("retained message");
        let Message::User(retained_user) = retained_message else {
            panic!("retained user")
        };
        assert_eq!(retained_user.content.last(), Some(&image));
    }

    #[test]
    fn pre_compaction_rewrites_oversized_recent_tool_output() {
        let session_id = Uuid::now_v7();
        let call = tool_call_assistant("assistant-call", "call-large");
        let output = tool_result("result-large", "call-large", &"z".repeat(1_100_000));
        let request = form_compaction_request(
            &config("openai"),
            session_id,
            &[
                session_message(session_id, 1, call),
                session_message(session_id, 2, output),
            ],
        )
        .expect("compaction request");
        let result = request
            .messages
            .iter()
            .find_map(|message| match message {
                Message::ToolResult(result) => Some(result),
                _ => None,
            })
            .expect("tool result");
        assert_eq!(
            result.content,
            vec![ContentPart::Text(TextContent {
                content: super::CONTEXT_WINDOW_TRUNCATED_OUTPUT_MESSAGE.to_owned(),
                metadata: None,
            })]
        );
    }

    #[test]
    fn estimator_uses_utf8_bytes_and_discounts_opaque_reasoning() {
        assert_eq!(estimate_text_tokens("éééé"), 2);
        let encrypted = "A".repeat(1_868);
        let expected_visible_bytes = (1_868_u64 * 3 / 4).saturating_sub(650);
        assert_eq!(
            estimate_native_item_tokens(&json!({
                "type": "reasoning",
                "encrypted_content": encrypted,
                "summary": [],
            })),
            expected_visible_bytes.div_ceil(4)
        );
    }

    #[test]
    fn automatic_redelivery_does_not_compact_twice() {
        let session_id = Uuid::now_v7();
        let run_id = Uuid::now_v7();
        let mut request =
            form_compaction_request(&config("openai"), session_id, &[]).expect("request");
        let mut response = compaction_response("openai");
        response.usage = Some(Usage {
            input: Some(244_800),
            output: None,
            cache_read: None,
            cache_write: None,
            cost: None,
        });
        request.messages.push(Message::Assistant(response));
        let mut checkpoint = session_message(
            session_id,
            1,
            Message::Custom(
                checkpoint_from_response(
                    &config("openai"),
                    message_id("checkpoint"),
                    Timestamp(1),
                    CodexCompactionTrigger::AutomaticLimit,
                    &[],
                    &[],
                    &compaction_response("openai"),
                )
                .expect("checkpoint"),
            ),
        );
        checkpoint.run_id = Some(run_id);
        checkpoint.turn_number = Some(3);
        assert!(matches!(
            plan_compaction(&request, &[checkpoint], 272_000, false, run_id, 3),
            CompactionPlan::AlreadyCompactedForTurn { .. }
        ));
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

    fn system(id: &str, content: &str) -> Message {
        Message::System(SystemMessage {
            id: message_id(id),
            timestamp: Timestamp(1),
            content: vec![TextContent {
                content: content.to_owned(),
                metadata: None,
            }],
        })
    }

    fn tool_call_assistant(id: &str, call_id: &str) -> Message {
        let mut response = compaction_response("openai");
        response.id = message_id(id);
        response.stop_reason = StopReason::ToolUse;
        response.content = vec![AssistantContent::ToolCall {
            name: "exec".to_owned(),
            arguments: ToolArguments::String("text('done')".to_owned()),
            tool_call_id: ToolCallId::new(call_id).expect("tool call ID"),
        }];
        response.native_message = json!({
            "output": [{
                "type": "custom_tool_call",
                "call_id": call_id,
                "name": "exec",
                "input": "text('done')"
            }]
        });
        Message::Assistant(response)
    }

    fn tool_result(id: &str, call_id: &str, content: &str) -> Message {
        Message::ToolResult(ToolResultMessage {
            id: message_id(id),
            tool_name: "exec".to_owned(),
            tool_call_id: ToolCallId::new(call_id).expect("tool call ID"),
            content: vec![ContentPart::Text(TextContent {
                content: content.to_owned(),
                metadata: None,
            })],
            details: None,
            timestamp: Timestamp(1),
            outcome: ToolResultOutcome::Success,
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
