use std::collections::BTreeMap;

use llm_contracts::{
    AssistantContent, ContentPart, CustomMessage, LlmRequest, Message, MessageId, TextContent,
    Timestamp, ToolArguments, Usage, UserMessage,
};
use serde::{Deserialize, Serialize};
use serde_json::json;

use super::{
    context_formation::estimate_message_tokens,
    model_resolver::{ModelResolverError, get_model_config},
};

pub const PI_COMPACTION_MESSAGE_TAG: &str = "pi.compaction";

pub(crate) const COMPACTION_SUMMARY_PREFIX: &str = "The conversation history before this point was compacted into the following summary:\n\n<summary>\n";
pub(crate) const COMPACTION_SUMMARY_SUFFIX: &str = "\n</summary>";
const TOOL_RESULT_MAX_CHARS: usize = 2_000;
const COMPACTION_MAX_OUTPUT_TOKENS: u64 = 13_107;
const KEEP_RECENT_TOKENS: u64 = 20_000;

const COMPACTION_SYSTEM_PROMPT: &str = r#"You are a context summarization assistant. Your task is to read a conversation between a user and an AI assistant, then produce a structured summary following the exact format specified.

Do NOT continue the conversation. Do NOT respond to any questions in the conversation. ONLY output the structured summary."#;

const SUMMARIZATION_PROMPT: &str = r#"The messages above are a conversation to summarize. Create a structured context checkpoint summary that another LLM will use to continue the work.

Use this EXACT format:

## Goal
[What is the user trying to accomplish? Can be multiple items if the session covers different tasks.]

## Constraints & Preferences
- [Any constraints, preferences, or requirements mentioned by user]
- [Or "(none)" if none were mentioned]

## Progress
### Done
- [x] [Completed tasks/changes]

### In Progress
- [ ] [Current work]

### Blocked
- [Issues preventing progress, if any]

## Key Decisions
- **[Decision]**: [Brief rationale]

## Next Steps
1. [Ordered list of what should happen next]

## Critical Context
- [Any data, examples, or references needed to continue]
- [Or "(none)" if not applicable]

Keep each section concise. Preserve exact file paths, function names, and error messages."#;

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct PiCompactionMessageContent {
    pub summary: String,
    pub first_kept_message_id: MessageId,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub usage: Option<Usage>,
}

pub fn generate_compaction_system_prompt() -> String {
    COMPACTION_SYSTEM_PROMPT.to_owned()
}

pub fn create_pi_compaction_message(
    id: MessageId,
    timestamp: Timestamp,
    compaction: PiCompactionMessageContent,
) -> Result<CustomMessage, serde_json::Error> {
    let content = serde_json::to_value(compaction)?;
    let serde_json::Value::Object(content) = content else {
        unreachable!("PiCompactionMessageContent serializes as an object")
    };

    Ok(CustomMessage {
        id,
        content,
        tag: Some(PI_COMPACTION_MESSAGE_TAG.to_owned()),
        timestamp,
    })
}

pub fn serialize_message_history(messages: &[Message]) -> String {
    let mut serialized = Vec::new();

    if let Some((compaction_index, compaction)) = latest_compaction(messages) {
        serialized.push(format!(
            "[User]: {COMPACTION_SUMMARY_PREFIX}{}{COMPACTION_SUMMARY_SUFFIX}",
            compaction.summary
        ));

        let first_kept_index = messages[..compaction_index]
            .iter()
            .position(|message| message_id(message) == &compaction.first_kept_message_id)
            .unwrap_or(0);

        for message in &messages[first_kept_index..compaction_index] {
            serialize_message(message, &mut serialized);
        }
        for message in &messages[compaction_index + 1..] {
            serialize_message(message, &mut serialized);
        }
    } else {
        for message in messages {
            serialize_message(message, &mut serialized);
        }
    }

    serialized.join("\n\n")
}

pub fn form_compaction_request(
    messages: &[Message],
    provider: &str,
    model_id: &str,
    reasoning_level: &str,
) -> Result<LlmRequest, ModelResolverError> {
    let config = get_model_config(provider, model_id, reasoning_level, "")?;
    let mut provider_options = config.provider_options;
    provider_options.remove("prompt_cache_key");
    provider_options.insert(
        "prompt_cache_options".to_owned(),
        json!({ "mode": "explicit" }),
    );
    provider_options.insert(
        "max_output_tokens".to_owned(),
        json!(COMPACTION_MAX_OUTPUT_TOKENS),
    );

    let conversation = serialize_message_history(messages);
    let content =
        format!("<conversation>\n{conversation}\n</conversation>\n\n{SUMMARIZATION_PROMPT}");
    let timestamp = messages
        .last()
        .map(message_timestamp)
        .unwrap_or(Timestamp(0));

    Ok(LlmRequest {
        model: config.model,
        instructions: Some(generate_compaction_system_prompt()),
        messages: vec![Message::User(UserMessage {
            id: MessageId::new("pi-compaction-request").expect("static message id is valid"),
            timestamp,
            content: vec![ContentPart::Text(TextContent {
                content,
                metadata: None,
            })],
        })],
        tools: Vec::new(),
        provider_options,
        metadata: BTreeMap::new(),
    })
}

pub(crate) fn select_first_kept_message_id(messages: &[Message]) -> Option<MessageId> {
    let visible = visible_message_indices(messages);
    let mut kept_tokens = 0_u64;
    let mut first_kept = None;
    for index in visible.into_iter().rev() {
        let message = &messages[index];
        kept_tokens = kept_tokens.saturating_add(estimate_message_tokens(message));
        if is_valid_cut_point(message) {
            first_kept = Some(message_id(message).clone());
            if kept_tokens >= KEEP_RECENT_TOKENS {
                break;
            }
        }
    }
    first_kept
}

fn visible_message_indices(messages: &[Message]) -> Vec<usize> {
    let Some((compaction_index, compaction)) = latest_compaction(messages) else {
        return messages
            .iter()
            .enumerate()
            .filter_map(|(index, message)| (!is_pi_compaction(message)).then_some(index))
            .collect();
    };
    let first_kept = messages[..compaction_index]
        .iter()
        .position(|message| message_id(message) == &compaction.first_kept_message_id)
        .unwrap_or(compaction_index + 1);
    (first_kept..messages.len())
        .filter(|index| *index != compaction_index && !is_pi_compaction(&messages[*index]))
        .collect()
}

fn is_valid_cut_point(message: &Message) -> bool {
    !matches!(message, Message::ToolResult(_)) && !is_pi_compaction(message)
}

fn is_pi_compaction(message: &Message) -> bool {
    matches!(
        message,
        Message::Custom(message) if message.tag.as_deref() == Some(PI_COMPACTION_MESSAGE_TAG)
    )
}

pub(crate) fn latest_compaction(
    messages: &[Message],
) -> Option<(usize, PiCompactionMessageContent)> {
    messages
        .iter()
        .enumerate()
        .rev()
        .find_map(|(index, message)| {
            let Message::Custom(message) = message else {
                return None;
            };
            if message.tag.as_deref() != Some(PI_COMPACTION_MESSAGE_TAG) {
                return None;
            }

            serde_json::from_value(serde_json::Value::Object(message.content.clone()))
                .ok()
                .map(|compaction| (index, compaction))
        })
}

pub(crate) fn message_id(message: &Message) -> &MessageId {
    match message {
        Message::User(message) => &message.id,
        Message::System(message) => &message.id,
        Message::ToolResult(message) => &message.id,
        Message::Assistant(message) => &message.id,
        Message::Custom(message) => &message.id,
    }
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

fn serialize_message(message: &Message, serialized: &mut Vec<String>) {
    match message {
        Message::User(message) => {
            let text = content_text(&message.content, "");
            if !text.is_empty() {
                serialized.push(format!("[User]: {text}"));
            }
        }
        Message::Assistant(message) => serialize_assistant_content(&message.content, serialized),
        Message::ToolResult(message) => {
            let text = content_text(&message.content, "");
            if !text.is_empty() {
                serialized.push(format!("[Tool result]: {}", truncate_tool_result(&text)));
            }
        }
        Message::System(_) | Message::Custom(_) => {}
    }
}

fn serialize_assistant_content(content: &[AssistantContent], serialized: &mut Vec<String>) {
    let thinking = content
        .iter()
        .filter_map(|part| match part {
            AssistantContent::Thinking { thinking_text } => Some(thinking_text.as_str()),
            _ => None,
        })
        .collect::<Vec<_>>()
        .join("\n");
    if !thinking.is_empty() {
        serialized.push(format!("[Assistant thinking]: {thinking}"));
    }

    let response = content
        .iter()
        .filter_map(|part| match part {
            AssistantContent::Response { response } => Some(response.content.as_str()),
            _ => None,
        })
        .collect::<Vec<_>>()
        .join("\n");
    if !response.is_empty() {
        serialized.push(format!("[Assistant]: {response}"));
    }

    let tool_calls = content
        .iter()
        .filter_map(|part| match part {
            AssistantContent::ToolCall {
                name, arguments, ..
            } => Some(format_tool_call(name, arguments)),
            _ => None,
        })
        .collect::<Vec<_>>()
        .join("; ");
    if !tool_calls.is_empty() {
        serialized.push(format!("[Assistant tool calls]: {tool_calls}"));
    }
}

fn content_text(content: &[ContentPart], separator: &str) -> String {
    content
        .iter()
        .filter_map(|part| match part {
            ContentPart::Text(text) => Some(text.content.as_str()),
            ContentPart::Image(_) => None,
        })
        .collect::<Vec<_>>()
        .join(separator)
}

fn format_tool_call(name: &str, arguments: &ToolArguments) -> String {
    match arguments {
        ToolArguments::Object(arguments) => {
            let arguments = arguments
                .iter()
                .map(|(key, value)| format!("{key}={value}"))
                .collect::<Vec<_>>()
                .join(", ");
            format!("{name}({arguments})")
        }
        ToolArguments::String(arguments) => format!("{name}({arguments})"),
    }
}

fn truncate_tool_result(text: &str) -> String {
    let character_count = text.chars().count();
    if character_count <= TOOL_RESULT_MAX_CHARS {
        return text.to_owned();
    }

    let text = text.chars().take(TOOL_RESULT_MAX_CHARS).collect::<String>();
    let truncated_characters = character_count - TOOL_RESULT_MAX_CHARS;
    format!("{text}\n\n[... {truncated_characters} more characters truncated]")
}

#[cfg(test)]
mod tests {
    use llm_contracts::{ContentPart, Message, MessageId, Timestamp, Validate};
    use serde_json::json;

    use crate::harness::model_resolver::ModelResolverError;

    use super::{
        COMPACTION_MAX_OUTPUT_TOKENS, COMPACTION_SYSTEM_PROMPT, PI_COMPACTION_MESSAGE_TAG,
        PiCompactionMessageContent, create_pi_compaction_message, form_compaction_request,
        generate_compaction_system_prompt, serialize_message_history,
    };

    #[test]
    fn returns_pi_compaction_system_prompt() {
        assert_eq!(
            generate_compaction_system_prompt(),
            COMPACTION_SYSTEM_PROMPT
        );
    }

    #[test]
    fn serializes_compaction_message_content() {
        let first_kept_message_id = message_id("user-1");
        let message = create_pi_compaction_message(
            message_id("compaction-1"),
            Timestamp(1_000),
            PiCompactionMessageContent {
                summary: "## Goal\nContinue the implementation.".to_owned(),
                first_kept_message_id,
                usage: None,
            },
        )
        .expect("compaction message is created");

        assert_eq!(PI_COMPACTION_MESSAGE_TAG, "pi.compaction");
        assert_eq!(
            serde_json::to_value(Message::Custom(message)).expect("compaction message serializes"),
            json!({
                "role": "custom",
                "id": "compaction-1",
                "tag": "pi.compaction",
                "timestamp": 1_000,
                "content": {
                    "summary": "## Goal\nContinue the implementation.",
                    "first_kept_message_id": "user-1"
                }
            })
        );
    }

    #[test]
    fn serializes_conversation_messages_for_compaction() {
        let messages = vec![
            message(json!({
                "role": "system",
                "id": "system-1",
                "timestamp": 1,
                "content": [{"content": "Do not include this."}]
            })),
            message(json!({
                "role": "user",
                "id": "user-1",
                "timestamp": 2,
                "content": [
                    {"type": "text", "content": "Fix "},
                    {"type": "image", "source": {"type": "url", "url": "https://example.com/image.png"}},
                    {"type": "text", "content": "the bug."}
                ]
            })),
            message(json!({
                "role": "assistant",
                "id": "assistant-1",
                "model": {"provider": "openai", "id": "gpt-5.6-sol"},
                "duration_ms": 10,
                "native_message": {},
                "content": [
                    {"type": "thinking", "thinking_text": "I should inspect it."},
                    {"type": "response", "response": {"content": "I will inspect the file."}},
                    {
                        "type": "tool_call",
                        "name": "read",
                        "arguments": {"path": "src/lib.rs", "offset": 1},
                        "tool_call_id": "call-1"
                    }
                ],
                "stop_reason": "tool_use",
                "timestamp": 3
            })),
            message(json!({
                "role": "tool_result",
                "id": "tool-1",
                "tool_name": "read",
                "tool_call_id": "call-1",
                "content": [{"type": "text", "content": "file contents"}],
                "timestamp": 4,
                "outcome": {"status": "success"}
            })),
            message(json!({
                "role": "custom",
                "id": "custom-1",
                "tag": "application.state",
                "timestamp": 5,
                "content": {"value": true}
            })),
        ];

        assert_eq!(
            serialize_message_history(&messages),
            "[User]: Fix the bug.\n\n\
[Assistant thinking]: I should inspect it.\n\n\
[Assistant]: I will inspect the file.\n\n\
[Assistant tool calls]: read(offset=1, path=\"src/lib.rs\")\n\n\
[Tool result]: file contents"
        );
    }

    #[test]
    fn uses_only_the_latest_compaction_and_its_kept_history() {
        let messages = vec![
            user_message("old", "Discarded original history."),
            user_message("kept", "Retained work."),
            compaction_message("compaction-1", "Old summary", "kept"),
            user_message("later", "Work after the first compaction."),
            compaction_message("compaction-2", "Current summary", "kept"),
            user_message("new", "Work after the current compaction."),
        ];

        assert_eq!(
            serialize_message_history(&messages),
            "[User]: The conversation history before this point was compacted into the following summary:\n\n\
<summary>\n\
Current summary\n\
</summary>\n\n\
[User]: Retained work.\n\n\
[User]: Work after the first compaction.\n\n\
[User]: Work after the current compaction."
        );
    }

    #[test]
    fn truncates_large_tool_results() {
        let output = "x".repeat(2_005);
        let messages = vec![message(json!({
            "role": "tool_result",
            "id": "tool-1",
            "tool_name": "bash",
            "tool_call_id": "call-1",
            "content": [{"type": "text", "content": output}],
            "timestamp": 1,
            "outcome": {"status": "success"}
        }))];

        let expected = format!(
            "[Tool result]: {}\n\n[... 5 more characters truncated]",
            "x".repeat(2_000)
        );
        assert_eq!(serialize_message_history(&messages), expected);
    }

    #[test]
    fn forms_a_complete_uncached_compaction_request() {
        let messages = vec![user_message("user-1", "Fix the parser.")];

        let request = form_compaction_request(&messages, "openai", "gpt-5.6-terra", "high")
            .expect("supported compaction request");

        request.validate().expect("valid LLM request");
        assert_eq!(request.model.provider.as_str(), "openai");
        assert_eq!(request.model.id.as_str(), "gpt-5.6-terra");
        assert_eq!(
            request.instructions.as_deref(),
            Some(COMPACTION_SYSTEM_PROMPT)
        );
        assert!(request.tools.is_empty());
        assert!(request.metadata.is_empty());
        assert_eq!(request.provider_options["store"], json!(false));
        assert_eq!(
            request.provider_options["reasoning"],
            json!({"effort": "high", "summary": "auto"})
        );
        assert_eq!(
            request.provider_options["include"],
            json!(["reasoning.encrypted_content"])
        );
        assert_eq!(
            request.provider_options["prompt_cache_options"],
            json!({"mode": "explicit"})
        );
        assert_eq!(
            request.provider_options["max_output_tokens"],
            json!(COMPACTION_MAX_OUTPUT_TOKENS)
        );
        assert!(!request.provider_options.contains_key("prompt_cache_key"));

        let [Message::User(user_message)] = request.messages.as_slice() else {
            panic!("compaction request has one user message")
        };
        assert_eq!(user_message.id.as_str(), "pi-compaction-request");
        assert_eq!(user_message.timestamp, Timestamp(1));
        let [ContentPart::Text(content)] = user_message.content.as_slice() else {
            panic!("compaction user message has one text part")
        };
        assert!(
            content
                .content
                .starts_with("<conversation>\n[User]: Fix the parser.\n</conversation>\n\n")
        );
        assert!(content.content.contains("## Goal"));
        assert!(content.content.contains("## Next Steps"));
    }

    #[test]
    fn rejects_an_unsupported_compaction_model() {
        assert_eq!(
            form_compaction_request(&[], "openai", "gpt-5.5", "medium"),
            Err(ModelResolverError::UnsupportedModel {
                provider: "openai".to_owned(),
                model_id: "gpt-5.5".to_owned(),
            })
        );
    }

    fn compaction_message(id: &str, summary: &str, first_kept_id: &str) -> Message {
        Message::Custom(
            create_pi_compaction_message(
                message_id(id),
                Timestamp(1),
                PiCompactionMessageContent {
                    summary: summary.to_owned(),
                    first_kept_message_id: message_id(first_kept_id),
                    usage: None,
                },
            )
            .expect("compaction message is created"),
        )
    }

    fn user_message(id: &str, content: &str) -> Message {
        message(json!({
            "role": "user",
            "id": id,
            "timestamp": 1,
            "content": [{"type": "text", "content": content}]
        }))
    }

    fn message(value: serde_json::Value) -> Message {
        serde_json::from_value(value).expect("valid message")
    }

    fn message_id(value: &str) -> MessageId {
        MessageId::new(value).expect("valid message id")
    }
}
