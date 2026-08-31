use llm_contracts::{
    AssistantContent, ContentPart, Message, TextContent, ToolArguments, Usage, UserMessage,
};

use super::{
    compact::{
        COMPACTION_SUMMARY_PREFIX, COMPACTION_SUMMARY_SUFFIX, ENVIRONMENT_COMPACTION_MESSAGE_TAG,
        latest_compaction, message_id,
    },
    model_catalog::{find_model, supports_provider},
};

const COMPACTION_RESERVE_TOKENS: u64 = 16_384;
const ESTIMATED_IMAGE_CHARS: u64 = 4_800;

#[derive(Debug, Eq, PartialEq, thiserror::Error)]
pub enum ContextFormationError {
    #[error("unsupported provider {0:?}")]
    UnsupportedProvider(String),
    #[error("unsupported model {model_id:?} for provider {provider:?}")]
    UnsupportedModel { provider: String, model_id: String },
    #[error(
        "compaction required: estimated context is {estimated_tokens} tokens and the model context window is {context_window} tokens"
    )]
    CompactionRequired {
        estimated_tokens: u64,
        context_window: u64,
    },
}

pub fn form_context_messages(
    messages: &[Message],
    provider: &str,
    model_id: &str,
) -> Result<Vec<Message>, ContextFormationError> {
    let context_window = model_context_window(provider, model_id)?;
    let context = active_context(messages);
    let estimated_tokens = estimate_context_tokens(&context.messages, context.usage_start);

    if estimated_tokens > context_window.saturating_sub(COMPACTION_RESERVE_TOKENS) {
        return Err(ContextFormationError::CompactionRequired {
            estimated_tokens,
            context_window,
        });
    }

    Ok(context.messages)
}

struct ActiveContext {
    messages: Vec<Message>,
    usage_start: usize,
}

fn active_context(messages: &[Message]) -> ActiveContext {
    let Some((compaction_index, compaction)) = latest_compaction(messages) else {
        return ActiveContext {
            messages: messages
                .iter()
                .filter(|message| !is_environment_compaction(message))
                .cloned()
                .collect(),
            usage_start: 0,
        };
    };

    let Message::Custom(compaction_message) = &messages[compaction_index] else {
        unreachable!("latest_compaction only returns custom messages")
    };
    let summary = format!(
        "{COMPACTION_SUMMARY_PREFIX}{}{COMPACTION_SUMMARY_SUFFIX}",
        compaction.summary
    );
    let mut context = vec![Message::User(UserMessage {
        id: compaction_message.id.clone(),
        timestamp: compaction_message.timestamp,
        content: vec![ContentPart::Text(TextContent {
            content: summary,
            metadata: None,
        })],
    })];

    let first_kept_index = messages[..compaction_index]
        .iter()
        .position(|message| message_id(message) == &compaction.first_kept_message_id)
        .unwrap_or(0);
    context.extend(
        messages[first_kept_index..compaction_index]
            .iter()
            .filter(|message| !is_environment_compaction(message))
            .cloned(),
    );

    let usage_start = context.len();
    context.extend(
        messages[compaction_index + 1..]
            .iter()
            .filter(|message| !is_environment_compaction(message))
            .cloned(),
    );

    ActiveContext {
        messages: context,
        usage_start,
    }
}

fn model_context_window(provider: &str, model_id: &str) -> Result<u64, ContextFormationError> {
    if !supports_provider(provider) {
        return Err(ContextFormationError::UnsupportedProvider(
            provider.to_owned(),
        ));
    }

    find_model(provider, model_id)
        .map(|model| model.context_window)
        .ok_or_else(|| ContextFormationError::UnsupportedModel {
            provider: provider.to_owned(),
            model_id: model_id.to_owned(),
        })
}

fn is_environment_compaction(message: &Message) -> bool {
    matches!(
        message,
        Message::Custom(message)
            if message.tag.as_deref() == Some(ENVIRONMENT_COMPACTION_MESSAGE_TAG)
    )
}

fn estimate_context_tokens(messages: &[Message], usage_start: usize) -> u64 {
    let usage = messages
        .iter()
        .enumerate()
        .skip(usage_start)
        .rev()
        .find_map(|(index, message)| match message {
            Message::Assistant(message) => message
                .usage
                .as_ref()
                .map(usage_tokens)
                .filter(|tokens| *tokens > 0)
                .map(|tokens| (index, tokens)),
            _ => None,
        });

    match usage {
        Some((index, tokens)) => messages[index + 1..].iter().fold(tokens, |total, message| {
            total.saturating_add(estimate_message_tokens(message))
        }),
        None => messages.iter().fold(0, |total, message| {
            total.saturating_add(estimate_message_tokens(message))
        }),
    }
}

fn usage_tokens(usage: &Usage) -> u64 {
    usage
        .input
        .unwrap_or(0)
        .saturating_add(usage.output.unwrap_or(0))
        .saturating_add(usage.cache_read.unwrap_or(0))
        .saturating_add(usage.cache_write.unwrap_or(0))
}

pub(crate) fn estimate_message_tokens(message: &Message) -> u64 {
    let characters = match message {
        Message::User(message) => content_characters(&message.content),
        Message::System(message) => message
            .content
            .iter()
            .map(|content| character_count(&content.content))
            .sum(),
        Message::ToolResult(message) => content_characters(&message.content),
        Message::Assistant(message) => message
            .content
            .iter()
            .map(assistant_content_characters)
            .sum(),
        Message::Custom(message) => json_object_characters(&message.content),
    };

    characters.saturating_add(3) / 4
}

fn content_characters(content: &[ContentPart]) -> u64 {
    content
        .iter()
        .map(|part| match part {
            ContentPart::Text(text) => character_count(&text.content),
            ContentPart::Image(_) => ESTIMATED_IMAGE_CHARS,
        })
        .sum()
}

fn assistant_content_characters(content: &AssistantContent) -> u64 {
    match content {
        AssistantContent::Response { response } => character_count(&response.content),
        AssistantContent::Thinking { thinking_text } => character_count(thinking_text),
        AssistantContent::ToolCall {
            name, arguments, ..
        } => character_count(name).saturating_add(tool_arguments_characters(arguments)),
    }
}

fn tool_arguments_characters(arguments: &ToolArguments) -> u64 {
    match arguments {
        ToolArguments::Object(arguments) => json_object_characters(arguments),
        ToolArguments::String(arguments) => character_count(arguments),
    }
}

fn json_object_characters(object: &llm_contracts::JsonObject) -> u64 {
    let serialized = serde_json::to_string(object).expect("JSON objects are serializable");
    character_count(&serialized)
}

fn character_count(value: &str) -> u64 {
    u64::try_from(value.chars().count()).unwrap_or(u64::MAX)
}

#[cfg(test)]
mod tests {
    use llm_contracts::{ContentPart, Message, Timestamp};
    use serde_json::json;

    use super::{
        COMPACTION_RESERVE_TOKENS, ContextFormationError, estimate_context_tokens,
        form_context_messages,
    };
    use crate::harness::{
        compact::{EnvironmentCompactionMessageContent, create_environment_compaction_message},
        model_catalog::find_model,
    };

    #[test]
    fn returns_the_full_context_without_compaction() {
        let messages = vec![
            user_message("user-1", "Inspect the parser."),
            assistant_message("assistant-1", "I will inspect it.", None),
        ];

        assert_eq!(
            form_context_messages(&messages, "openai", "gpt-5.6-sol").expect("context fits"),
            messages
        );
    }

    #[test]
    fn reconstructs_context_from_only_the_latest_compaction() {
        let messages = vec![
            user_message("old", "Discard this."),
            user_message("kept", "Keep this."),
            compaction_message("compaction-1", "Old summary", "kept"),
            assistant_message("assistant-1", "Retained response.", None),
            compaction_message("compaction-2", "Current summary", "kept"),
            user_message("new", "Continue from here."),
        ];

        let context = form_context_messages(&messages, "openai", "gpt-5.6-sol")
            .expect("compacted context fits");

        assert_eq!(context.len(), 4);
        let Message::User(summary) = &context[0] else {
            panic!("first context message is the compaction summary")
        };
        assert_eq!(summary.id.as_str(), "compaction-2");
        let [ContentPart::Text(summary)] = summary.content.as_slice() else {
            panic!("summary is text")
        };
        assert!(
            summary
                .content
                .contains("<summary>\nCurrent summary\n</summary>")
        );
        assert_eq!(message_id(&context[1]), "kept");
        assert_eq!(message_id(&context[2]), "assistant-1");
        assert_eq!(message_id(&context[3]), "new");
    }

    #[test]
    fn requires_compaction_when_usage_crosses_the_reserved_threshold() {
        let model = find_model("openai", "gpt-5.6-sol").expect("provider catalog model");
        let threshold = model.context_window - COMPACTION_RESERVE_TOKENS;
        let messages = vec![assistant_message(
            "assistant-1",
            "done",
            Some(threshold + 1),
        )];

        assert_eq!(
            form_context_messages(&messages, model.provider, model.id),
            Err(ContextFormationError::CompactionRequired {
                estimated_tokens: threshold + 1,
                context_window: model.context_window,
            })
        );
    }

    #[test]
    fn adds_trailing_message_estimates_to_the_last_usage() {
        let messages = vec![
            assistant_message("assistant-1", "done", Some(100)),
            user_message("user-1", "12345678"),
        ];

        assert_eq!(estimate_context_tokens(&messages, 0), 102);
    }

    #[test]
    fn ignores_stale_usage_from_before_the_latest_compaction() {
        let model = find_model("openai", "gpt-5.6-sol").expect("provider catalog model");
        let messages = vec![
            assistant_message("kept", "Old response.", Some(model.context_window)),
            compaction_message("compaction-1", "Small summary", "kept"),
            user_message("new", "Continue."),
        ];

        assert!(form_context_messages(&messages, model.provider, model.id).is_ok());
    }

    #[test]
    fn uses_the_chatgpt_provider_catalog_for_context_limits() {
        let messages = vec![user_message("user-1", "Inspect the parser.")];

        assert_eq!(
            form_context_messages(&messages, "chatgpt", "gpt-5.6-sol")
                .expect("ChatGPT context fits"),
            messages
        );
    }

    #[test]
    fn uses_the_deepseek_provider_catalog_for_context_limits() {
        let messages = vec![user_message("user-1", "Inspect the parser.")];

        assert_eq!(
            form_context_messages(&messages, "deepseek", "deepseek-v4-pro")
                .expect("DeepSeek context fits"),
            messages
        );
    }

    #[test]
    fn rejects_unknown_models() {
        assert_eq!(
            form_context_messages(&[], "openai", "gpt-5.5"),
            Err(ContextFormationError::UnsupportedModel {
                provider: "openai".to_owned(),
                model_id: "gpt-5.5".to_owned(),
            })
        );
    }

    fn compaction_message(id: &str, summary: &str, first_kept_id: &str) -> Message {
        Message::Custom(
            create_environment_compaction_message(
                id.parse().expect("valid message id"),
                Timestamp(1),
                EnvironmentCompactionMessageContent {
                    summary: summary.to_owned(),
                    first_kept_message_id: first_kept_id.parse().expect("valid message id"),
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

    fn assistant_message(id: &str, content: &str, input_tokens: Option<u64>) -> Message {
        message(json!({
            "role": "assistant",
            "id": id,
            "model": {"provider": "openai", "id": "gpt-5.6-sol"},
            "usage": input_tokens.map(|input| json!({"input": input})),
            "duration_ms": 1,
            "native_message": {},
            "content": [{"type": "response", "response": {"content": content}}],
            "stop_reason": "stop",
            "timestamp": 1
        }))
    }

    fn message(value: serde_json::Value) -> Message {
        serde_json::from_value(value).expect("valid message")
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
