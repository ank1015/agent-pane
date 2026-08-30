use llm_contracts::{AssistantContent, ContentPart, Message, SearchInput};
use serde_json::{Value, json};

use super::environment::is_environment_message;

const ASSISTANT_CONTEXT_TOKEN_LIMIT: usize = 1_000;
const APPROX_BYTES_PER_TOKEN: usize = 4;

/// Builds the same visible conversation tail Codex sends to standalone web
/// search: the previous user text message, up to 1k approximate tokens of the
/// assistant text that followed it, and the current user text message.
#[must_use]
pub fn recent_search_input(messages: &[Message]) -> Option<SearchInput> {
    let mut visible = messages
        .iter()
        .filter_map(visible_message)
        .collect::<Vec<_>>();
    retain_tail_from_last_two_users(&mut visible);
    truncate_assistant_text(&mut visible, ASSISTANT_CONTEXT_TOKEN_LIMIT);
    (!visible.is_empty()).then_some(SearchInput::Items(visible))
}

fn visible_message(message: &Message) -> Option<Value> {
    match message {
        Message::User(user) if !is_environment_message(message) => {
            let content = user
                .content
                .iter()
                .filter_map(|part| match part {
                    ContentPart::Text(text) => {
                        Some(json!({"type": "input_text", "text": text.content}))
                    }
                    ContentPart::Image(_) => None,
                })
                .collect::<Vec<_>>();
            (!content.is_empty()).then(|| {
                json!({
                    "type": "message",
                    "role": "user",
                    "content": content
                })
            })
        }
        Message::Assistant(assistant) => {
            let content = assistant
                .content
                .iter()
                .filter_map(|part| match part {
                    AssistantContent::Response { response } if !response.content.is_empty() => {
                        Some(json!({"type": "output_text", "text": response.content}))
                    }
                    AssistantContent::Response { .. }
                    | AssistantContent::Thinking { .. }
                    | AssistantContent::ToolCall { .. } => None,
                })
                .collect::<Vec<_>>();
            (!content.is_empty()).then(|| {
                json!({
                    "type": "message",
                    "role": "assistant",
                    "content": content
                })
            })
        }
        Message::User(_) | Message::System(_) | Message::ToolResult(_) | Message::Custom(_) => None,
    }
}

fn retain_tail_from_last_two_users(items: &mut Vec<Value>) {
    let Some(latest_user_index) = items.iter().rposition(is_user_message) else {
        items.clear();
        return;
    };
    items.truncate(latest_user_index.saturating_add(1));
    let earliest_index = items
        .iter()
        .enumerate()
        .rev()
        .filter(|(_, item)| is_user_message(item))
        .take(2)
        .last()
        .map(|(index, _)| index)
        .unwrap_or(latest_user_index);
    items.drain(..earliest_index);
}

fn is_user_message(item: &Value) -> bool {
    item.get("type").and_then(Value::as_str) == Some("message")
        && item.get("role").and_then(Value::as_str) == Some("user")
}

fn truncate_assistant_text(items: &mut Vec<Value>, max_tokens: usize) {
    let mut remaining = max_tokens;
    items.retain_mut(|item| {
        if item.get("role").and_then(Value::as_str) != Some("assistant") {
            return true;
        }
        let Some(content) = item.get_mut("content").and_then(Value::as_array_mut) else {
            return false;
        };
        content.retain_mut(|part| {
            let Some(text) = part.get("text").and_then(Value::as_str).map(str::to_owned) else {
                return true;
            };
            if remaining == 0 {
                return false;
            }
            let tokens = approx_token_count(&text);
            if tokens <= remaining {
                remaining = remaining.saturating_sub(tokens);
                return true;
            }
            let truncated = truncate_middle_tokens(&text, remaining);
            part["text"] = Value::String(truncated);
            remaining = 0;
            true
        });
        !content.is_empty()
    });
}

fn approx_token_count(text: &str) -> usize {
    text.len()
        .saturating_add(APPROX_BYTES_PER_TOKEN.saturating_sub(1))
        / APPROX_BYTES_PER_TOKEN
}

fn truncate_middle_tokens(text: &str, max_tokens: usize) -> String {
    let max_bytes = max_tokens.saturating_mul(APPROX_BYTES_PER_TOKEN);
    if max_tokens > 0 && text.len() <= max_bytes {
        return text.to_owned();
    }
    if max_bytes == 0 {
        return format!("…{} tokens truncated…", approx_token_count(text));
    }
    let left_budget = max_bytes / 2;
    let right_budget = max_bytes.saturating_sub(left_budget);
    let tail_start = text.len().saturating_sub(right_budget);
    let mut prefix_end = 0;
    let mut suffix_start = text.len();
    for (index, character) in text.char_indices() {
        let end = index.saturating_add(character.len_utf8());
        if end <= left_budget {
            prefix_end = end;
        } else if index >= tail_start && suffix_start == text.len() {
            suffix_start = index;
        }
    }
    if suffix_start < prefix_end {
        suffix_start = prefix_end;
    }
    // Codex reports the byte-budget estimate rather than the exact UTF-8
    // boundary-adjusted removal size.
    let removed = text.len().saturating_sub(max_bytes);
    format!(
        "{}…{} tokens truncated…{}",
        &text[..prefix_end],
        removed.saturating_add(APPROX_BYTES_PER_TOKEN.saturating_sub(1)) / APPROX_BYTES_PER_TOKEN,
        &text[suffix_start..]
    )
}

#[cfg(test)]
mod tests {
    use llm_contracts::{
        AssistantContent, AssistantMessage, ContentPart, Message, MessageId, ModelId, ModelRef,
        ProviderId, StopReason, TextContent, Timestamp, UserMessage,
    };
    use serde_json::json;

    use super::{recent_search_input, truncate_middle_tokens};

    #[test]
    fn keeps_previous_user_assistant_and_current_user_text_only() {
        let messages = vec![
            user("old user"),
            assistant("old assistant"),
            user("previous user"),
            assistant("previous assistant"),
            user("current user"),
            assistant("current commentary"),
        ];
        assert_eq!(
            serde_json::to_value(recent_search_input(&messages)).expect("input"),
            json!([
                {
                    "type": "message",
                    "role": "user",
                    "content": [{"type": "input_text", "text": "previous user"}]
                },
                {
                    "type": "message",
                    "role": "assistant",
                    "content": [{"type": "output_text", "text": "previous assistant"}]
                },
                {
                    "type": "message",
                    "role": "user",
                    "content": [{"type": "input_text", "text": "current user"}]
                }
            ])
        );
    }

    #[test]
    fn uses_codex_middle_truncation_marker_and_budget_split() {
        assert_eq!(
            truncate_middle_tokens("aaaaaaaaaaaaaaaa", 2),
            "aaaa…2 tokens truncated…aaaa"
        );
    }

    fn user(text: &str) -> Message {
        Message::User(UserMessage {
            id: MessageId::new(format!("user-{text}")).expect("ID"),
            timestamp: Timestamp(1),
            content: vec![ContentPart::Text(TextContent {
                content: text.to_owned(),
                metadata: None,
            })],
        })
    }

    fn assistant(text: &str) -> Message {
        Message::Assistant(AssistantMessage {
            id: MessageId::new(format!("assistant-{text}")).expect("ID"),
            model: ModelRef {
                provider: ProviderId::new("openai").expect("provider"),
                id: ModelId::new("gpt-5.6-sol").expect("model"),
                name: None,
            },
            usage: None,
            duration_ms: 1,
            native_message: json!({"output": []}),
            content: vec![AssistantContent::Response {
                response: TextContent {
                    content: text.to_owned(),
                    metadata: None,
                },
            }],
            stop_reason: StopReason::Stop,
            timestamp: Timestamp(1),
        })
    }
}
