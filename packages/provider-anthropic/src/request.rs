use llm_contracts::{
    AssistantContent, ContentPart, CustomMessage, FunctionTool, ImageSource, LlmError, LlmRequest,
    Message, TextContent, ToolArguments, ToolDefinition, ToolResultMessage, ToolResultOutcome,
    Validate,
};
use serde_json::{Map, Value, json};

use crate::{
    ANTHROPIC_PROVIDER,
    error::{invalid_model, invalid_request},
    find_model,
    models::AnthropicModel,
};

/// Tag for custom messages containing native Anthropic Messages input turns.
pub const ANTHROPIC_NATIVE_INPUT_TAG: &str = "anthropic.native_input";

/// Conservative output-token default when callers do not specify `max_tokens`.
pub const DEFAULT_ANTHROPIC_MAX_TOKENS: u64 = 8_192;

const ANTHROPIC_IMAGE_MEDIA_TYPES: &[&str] =
    &["image/jpeg", "image/png", "image/gif", "image/webp"];

/// Builds a non-streaming `POST /messages` JSON body.
pub fn build_message_request(request: &LlmRequest) -> Result<Value, LlmError> {
    request
        .validate()
        .map_err(|error| invalid_request(error.to_string()))?;
    if request.model.provider.as_str() != ANTHROPIC_PROVIDER {
        return Err(invalid_request(format!(
            "Anthropic provider cannot handle model provider `{}`.",
            request.model.provider
        )));
    }
    let model = find_model(request.model.id.as_str())
        .ok_or_else(|| invalid_model(request.model.id.as_str()))?;
    build_message_request_for_model(request, model)
}

pub(crate) fn build_message_request_for_model(
    request: &LlmRequest,
    model: &AnthropicModel,
) -> Result<Value, LlmError> {
    validate_system_message_placement(&request.messages)?;
    let mut body = request.provider_options.clone();
    let hosted_tools = match body.remove("tools") {
        None => Vec::new(),
        Some(Value::Array(tools)) => tools,
        Some(_) => {
            return Err(invalid_request(
                "Anthropic provider_options.tools must be an array.",
            ));
        }
    };
    let max_tokens = body
        .remove("max_tokens")
        .unwrap_or_else(|| json!(DEFAULT_ANTHROPIC_MAX_TOKENS));
    let max_tokens_number = max_tokens
        .as_u64()
        .filter(|tokens| *tokens > 0)
        .ok_or_else(|| invalid_request("Anthropic max_tokens must be a positive integer."))?;
    if max_tokens_number > model.max_tokens {
        return Err(invalid_request(format!(
            "Anthropic max_tokens cannot exceed {} for {}.",
            model.max_tokens, model.id
        )));
    }

    for owned in ["model", "messages", "stream", "system"] {
        body.remove(owned);
    }

    let mut tools = hosted_tools;
    tools.extend(
        request
            .tools
            .iter()
            .map(map_portable_tool)
            .collect::<Result<Vec<_>, _>>()?,
    );

    let mut messages = Vec::new();
    for message in &request.messages {
        messages.extend(map_message(message, &request.tools)?);
    }

    body.insert("model".into(), Value::String(model.id.to_owned()));
    body.insert("max_tokens".into(), max_tokens);
    body.insert("stream".into(), Value::Bool(false));
    if let Some(instructions) = &request.instructions {
        body.insert("system".into(), Value::String(instructions.clone()));
    }
    if !tools.is_empty() {
        body.insert("tools".into(), Value::Array(tools));
    }
    body.insert(
        "messages".into(),
        Value::Array(merge_adjacent_messages(messages)?),
    );
    Ok(Value::Object(body))
}

fn map_message(message: &Message, tools: &[ToolDefinition]) -> Result<Vec<Value>, LlmError> {
    match message {
        Message::User(message) => {
            let content = map_content(&message.content)?;
            Ok((!content.is_empty())
                .then(|| json!({ "role": "user", "content": content }))
                .into_iter()
                .collect())
        }
        Message::System(message) => Ok(vec![json!({
            "role": "system",
            "content": map_system_content(&message.content),
        })]),
        Message::Assistant(message) => map_assistant_message(message, tools),
        Message::ToolResult(message) => Ok(vec![json!({
            "role": "user",
            "content": [map_tool_result(message, tools)?],
        })]),
        Message::Custom(message) => map_custom_message(message),
    }
}

fn map_system_content(content: &[TextContent]) -> Vec<Value> {
    content
        .iter()
        .filter(|part| !part.content.is_empty())
        .map(|part| json!({ "type": "text", "text": part.content }))
        .collect()
}

fn validate_system_message_placement(messages: &[Message]) -> Result<(), LlmError> {
    let mut index = 0;
    while index < messages.len() {
        if !matches!(messages[index], Message::System(_)) {
            index += 1;
            continue;
        }
        let start = index;
        while index < messages.len() && matches!(messages[index], Message::System(_)) {
            index += 1;
        }
        if start == 0 {
            return Err(invalid_request(
                "Anthropic system messages cannot be first; use request.instructions instead.",
            ));
        }
        let follows_supported_turn = matches!(
            &messages[start - 1],
            Message::User(_) | Message::ToolResult(_)
        ) || matches!(
            &messages[start - 1],
            Message::Assistant(message) if assistant_ends_with_server_tool_result(message)
        );
        if !follows_supported_turn {
            return Err(invalid_request(
                "Anthropic system messages must follow a user message, tool result, or assistant server-tool result.",
            ));
        }
        if index < messages.len() && !matches!(messages[index], Message::Assistant(_)) {
            return Err(invalid_request(
                "Anthropic system messages must be last or followed by an assistant message.",
            ));
        }
    }
    Ok(())
}

fn assistant_ends_with_server_tool_result(message: &llm_contracts::AssistantMessage) -> bool {
    message.model.provider.as_str() == ANTHROPIC_PROVIDER
        && message
            .native_message
            .get("content")
            .and_then(Value::as_array)
            .and_then(|content| content.last())
            .and_then(|block| block.get("type"))
            .and_then(Value::as_str)
            .is_some_and(|kind| kind == "server_tool_result" || kind.ends_with("_tool_result"))
}

fn map_assistant_message(
    message: &llm_contracts::AssistantMessage,
    tools: &[ToolDefinition],
) -> Result<Vec<Value>, LlmError> {
    if message.model.provider.as_str() == ANTHROPIC_PROVIDER {
        let content = message
            .native_message
            .get("content")
            .and_then(Value::as_array)
            .cloned()
            .ok_or_else(|| {
                invalid_request("Anthropic assistant native_message must contain a content array.")
            })?;
        return Ok(vec![json!({ "role": "assistant", "content": content })]);
    }

    let mut content = Vec::new();
    for part in &message.content {
        match part {
            AssistantContent::Response { response } if !response.content.is_empty() => {
                content.push(json!({ "type": "text", "text": response.content }));
            }
            AssistantContent::Response { .. } | AssistantContent::Thinking { .. } => {}
            AssistantContent::ToolCall {
                name,
                arguments,
                tool_call_id,
            } => {
                find_function_tool(tools, name)?;
                content.push(json!({
                    "type": "tool_use",
                    "id": tool_call_id,
                    "name": name,
                    "input": normalize_tool_arguments(arguments),
                }));
            }
        }
    }
    Ok((!content.is_empty())
        .then(|| json!({ "role": "assistant", "content": content }))
        .into_iter()
        .collect())
}

fn map_custom_message(message: &CustomMessage) -> Result<Vec<Value>, LlmError> {
    if message.tag.as_deref() != Some(ANTHROPIC_NATIVE_INPUT_TAG) {
        return Err(invalid_request(format!(
            "Unsupported Anthropic custom message tag: {}.",
            message.tag.as_deref().unwrap_or("<missing>")
        )));
    }
    message
        .content
        .get("messages")
        .and_then(Value::as_array)
        .cloned()
        .ok_or_else(|| invalid_request("Anthropic native input content.messages must be an array."))
}

fn map_tool_result(
    message: &ToolResultMessage,
    tools: &[ToolDefinition],
) -> Result<Value, LlmError> {
    find_function_tool(tools, &message.tool_name)?;
    let content = if message
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
        Value::Array(map_content(&message.content)?)
    };
    let mut block = Map::from_iter([
        ("type".into(), json!("tool_result")),
        ("tool_use_id".into(), json!(message.tool_call_id)),
        (
            "is_error".into(),
            Value::Bool(matches!(message.outcome, ToolResultOutcome::Error { .. })),
        ),
    ]);
    if !matches!(&content, Value::String(value) if value.is_empty()) {
        block.insert("content".into(), content);
    }
    Ok(Value::Object(block))
}

fn map_content(content: &[ContentPart]) -> Result<Vec<Value>, LlmError> {
    let mut blocks = Vec::new();
    for part in content {
        match part {
            ContentPart::Text(text) if !text.content.is_empty() => {
                blocks.push(json!({ "type": "text", "text": text.content }));
            }
            ContentPart::Text(_) => {}
            ContentPart::Image(image) => match &image.source {
                ImageSource::Base64(source) => {
                    if !ANTHROPIC_IMAGE_MEDIA_TYPES.contains(&source.mime_type.as_str()) {
                        return Err(invalid_request(format!(
                            "Unsupported Anthropic image MIME type: {}.",
                            source.mime_type
                        )));
                    }
                    blocks.push(json!({
                        "type": "image",
                        "source": {
                            "type": "base64",
                            "media_type": source.mime_type,
                            "data": source.data,
                        },
                    }));
                }
                ImageSource::Url(source) => blocks.push(json!({
                    "type": "image",
                    "source": { "type": "url", "url": source.url },
                })),
            },
        }
    }
    Ok(blocks)
}

fn normalize_tool_arguments(arguments: &ToolArguments) -> Value {
    match arguments {
        ToolArguments::Object(value) => Value::Object(value.clone()),
        ToolArguments::String(value) => serde_json::from_str(value)
            .ok()
            .filter(Value::is_object)
            .unwrap_or_else(|| Value::String(value.clone())),
    }
}

fn find_function_tool<'a>(
    tools: &'a [ToolDefinition],
    name: &str,
) -> Result<&'a FunctionTool, LlmError> {
    let tool = tools
        .iter()
        .find(|tool| tool.name() == name)
        .ok_or_else(|| {
            invalid_request(format!(
                "Missing tool definition for Anthropic tool call: {name}."
            ))
        })?;
    match tool {
        ToolDefinition::Function(tool) => Ok(tool),
        ToolDefinition::Custom(_) => Err(invalid_request(format!(
            "Anthropic does not support portable custom tool: {name}."
        ))),
    }
}

fn map_portable_tool(tool: &ToolDefinition) -> Result<Value, LlmError> {
    match tool {
        ToolDefinition::Custom(tool) => Err(invalid_request(format!(
            "Anthropic does not support portable custom tool: {}.",
            tool.name
        ))),
        ToolDefinition::Function(tool) => {
            if tool.parameters.get("type").and_then(Value::as_str) != Some("object") {
                return Err(invalid_request(format!(
                    "Anthropic tool parameters must be an object schema: {}.",
                    tool.name
                )));
            }
            let mut value = Map::from_iter([
                ("name".into(), json!(tool.name)),
                ("description".into(), json!(tool.description)),
                (
                    "input_schema".into(),
                    Value::Object(tool.parameters.clone()),
                ),
            ]);
            if let Some(strict) = tool.strict {
                value.insert("strict".into(), Value::Bool(strict));
            }
            Ok(Value::Object(value))
        }
    }
}

fn merge_adjacent_messages(messages: Vec<Value>) -> Result<Vec<Value>, LlmError> {
    let mut merged: Vec<Value> = Vec::new();
    for message in messages {
        let object = message.as_object().ok_or_else(|| {
            invalid_request("Anthropic native input messages must be JSON objects.")
        })?;
        let role = object.get("role").and_then(Value::as_str).ok_or_else(|| {
            invalid_request("Anthropic native input message role must be a string.")
        })?;
        let content = content_blocks(object.get("content").ok_or_else(|| {
            invalid_request("Anthropic native input message content must be present.")
        })?)?;
        if content.is_empty() {
            continue;
        }
        if merged
            .last()
            .and_then(|previous| previous.get("role"))
            .and_then(Value::as_str)
            == Some(role)
        {
            merged
                .last_mut()
                .and_then(Value::as_object_mut)
                .and_then(|previous| previous.get_mut("content"))
                .and_then(Value::as_array_mut)
                .expect("merged messages always contain an array")
                .extend(content);
        } else {
            merged.push(json!({ "role": role, "content": content }));
        }
    }
    Ok(merged)
}

fn content_blocks(content: &Value) -> Result<Vec<Value>, LlmError> {
    match content {
        Value::String(text) => Ok(vec![json!({ "type": "text", "text": text })]),
        Value::Array(blocks) => Ok(blocks.clone()),
        _ => Err(invalid_request(
            "Anthropic native input message content must be a string or array.",
        )),
    }
}
