use llm_contracts::{
    AssistantContent, ContentPart, CustomMessage, FunctionTool, ImageDetail, ImageSource, LlmError,
    LlmRequest, Message, TextContent, ToolArguments, ToolDefinition, ToolResultMessage,
    ToolResultOutcome, Validate,
};
use serde_json::{Map, Value, json};

use crate::{
    FIREWORKS_PROVIDER,
    error::{invalid_model, invalid_request},
    find_model,
    models::FireworksModel,
};

/// Tag for custom messages containing native Fireworks Chat Completions messages.
pub const FIREWORKS_NATIVE_INPUT_TAG: &str = "fireworks.native_input";

/// Builds a non-streaming `POST /chat/completions` JSON body.
pub fn build_chat_completion_request(request: &LlmRequest) -> Result<Value, LlmError> {
    request
        .validate()
        .map_err(|error| invalid_request(error.to_string()))?;
    if request.model.provider.as_str() != FIREWORKS_PROVIDER {
        return Err(invalid_request(format!(
            "Fireworks provider cannot handle model provider `{}`.",
            request.model.provider
        )));
    }
    let model = find_model(request.model.id.as_str())
        .ok_or_else(|| invalid_model(request.model.id.as_str()))?;
    build_chat_completion_request_for_model(request, model)
}

pub(crate) fn build_chat_completion_request_for_model(
    request: &LlmRequest,
    model: &FireworksModel,
) -> Result<Value, LlmError> {
    reject_nonstandard_pricing_options(&request.provider_options)?;
    let mut body = request.provider_options.clone();
    for owned in ["model", "messages", "tools", "stream", "n"] {
        body.remove(owned);
    }

    let tools = request
        .tools
        .iter()
        .map(map_portable_tool)
        .collect::<Result<Vec<_>, _>>()?;
    let mut messages = Vec::new();
    if let Some(instructions) = &request.instructions {
        messages.push(json!({ "role": "system", "content": instructions }));
    }
    for message in &request.messages {
        messages.extend(map_message(message, &request.tools)?);
    }

    body.insert("model".into(), Value::String(model.id.to_owned()));
    body.insert("stream".into(), Value::Bool(false));
    body.insert("n".into(), Value::from(1));
    if body.contains_key("prompt_token_ids") {
        body.remove("messages");
    } else {
        body.insert("messages".into(), Value::Array(messages));
    }
    if !tools.is_empty() {
        body.insert("tools".into(), Value::Array(tools));
    }
    Ok(Value::Object(body))
}

fn reject_nonstandard_pricing_options(options: &Map<String, Value>) -> Result<(), LlmError> {
    if options.get("service_tier").and_then(Value::as_str) == Some("priority") {
        return Err(invalid_request(
            "Fireworks service_tier `priority` is not supported because this package's catalog guarantees standard-tier pricing.",
        ));
    }
    Ok(())
}

fn map_message(message: &Message, tools: &[ToolDefinition]) -> Result<Vec<Value>, LlmError> {
    match message {
        Message::User(message) => Ok(vec![json!({
            "role": "user",
            "content": map_content(&message.content),
        })]),
        Message::System(message) => Ok(vec![json!({
            "role": "system",
            "content": map_system_content(&message.content),
        })]),
        Message::Assistant(message) => map_assistant_message(message, tools),
        Message::ToolResult(message) => Ok(vec![map_tool_result(message, tools)?]),
        Message::Custom(message) => map_custom_message(message),
    }
}

fn map_system_content(content: &[TextContent]) -> Vec<Value> {
    content
        .iter()
        .map(|part| json!({ "type": "text", "text": part.content }))
        .collect()
}

fn map_assistant_message(
    message: &llm_contracts::AssistantMessage,
    tools: &[ToolDefinition],
) -> Result<Vec<Value>, LlmError> {
    if message.model.provider.as_str() == FIREWORKS_PROVIDER {
        let native_message = message
            .native_message
            .get("choices")
            .and_then(Value::as_array)
            .and_then(|choices| {
                choices
                    .iter()
                    .find(|choice| choice.get("index").and_then(Value::as_u64) == Some(0))
            })
            .and_then(|choice| choice.get("message"))
            .filter(|message| message.is_object())
            .cloned()
            .ok_or_else(|| {
                invalid_request(
                    "Fireworks assistant native_message must contain choice zero with a message object.",
                )
            })?;
        return Ok(vec![native_message]);
    }

    let mut text = Vec::new();
    let mut reasoning = Vec::new();
    let mut tool_calls = Vec::new();
    for part in &message.content {
        match part {
            AssistantContent::Response { response } => text.push(response.content.as_str()),
            AssistantContent::Thinking { thinking_text } => reasoning.push(thinking_text.as_str()),
            AssistantContent::ToolCall {
                name,
                arguments,
                tool_call_id,
            } => {
                find_function_tool(tools, name)?;
                tool_calls.push(json!({
                    "id": tool_call_id,
                    "type": "function",
                    "function": {
                        "name": name,
                        "arguments": stringify_tool_arguments(arguments)?,
                    },
                }));
            }
        }
    }
    if text.is_empty() && reasoning.is_empty() && tool_calls.is_empty() {
        return Ok(Vec::new());
    }
    let mut native = Map::from_iter([("role".into(), json!("assistant"))]);
    if !text.is_empty() {
        native.insert("content".into(), json!(text.join("")));
    }
    if !reasoning.is_empty() {
        native.insert("reasoning_content".into(), json!(reasoning.join("")));
    }
    if !tool_calls.is_empty() {
        native.insert("tool_calls".into(), Value::Array(tool_calls));
    }
    Ok(vec![Value::Object(native)])
}

fn map_custom_message(message: &CustomMessage) -> Result<Vec<Value>, LlmError> {
    if message.tag.as_deref() != Some(FIREWORKS_NATIVE_INPUT_TAG) {
        return Err(invalid_request(format!(
            "Unsupported Fireworks custom message tag: {}.",
            message.tag.as_deref().unwrap_or("<missing>")
        )));
    }
    message
        .content
        .get("messages")
        .and_then(Value::as_array)
        .cloned()
        .ok_or_else(|| {
            invalid_request("Fireworks native input message content.messages must be an array.")
        })
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
        let text = message
            .content
            .iter()
            .filter_map(|part| match part {
                ContentPart::Text(text) => Some(text.content.as_str()),
                ContentPart::Image(_) => None,
            })
            .collect::<Vec<_>>()
            .join("\n");
        Value::String(
            if matches!(message.outcome, ToolResultOutcome::Error { .. }) {
                format!("[TOOL ERROR] {text}")
            } else {
                text
            },
        )
    } else {
        Value::Array(map_content(&message.content))
    };
    Ok(json!({
        "role": "tool",
        "tool_call_id": message.tool_call_id,
        "content": content,
    }))
}

fn map_content(content: &[ContentPart]) -> Vec<Value> {
    content
        .iter()
        .map(|part| match part {
            ContentPart::Text(text) => json!({ "type": "text", "text": text.content }),
            ContentPart::Image(image) => {
                let url = match &image.source {
                    ImageSource::Base64(source) => {
                        format!("data:{};base64,{}", source.mime_type, source.data)
                    }
                    ImageSource::Url(source) => source.url.clone(),
                };
                let mut image_url = Map::from_iter([("url".into(), json!(url))]);
                if let Some(detail) = image.detail.filter(|detail| *detail != ImageDetail::Auto) {
                    image_url.insert("detail".into(), json!(image_detail(detail)));
                }
                json!({ "type": "image_url", "image_url": image_url })
            }
        })
        .collect()
}

const fn image_detail(detail: ImageDetail) -> &'static str {
    match detail {
        ImageDetail::Auto => "auto",
        ImageDetail::Low => "low",
        ImageDetail::High | ImageDetail::Original => "high",
    }
}

fn map_portable_tool(tool: &ToolDefinition) -> Result<Value, LlmError> {
    match tool {
        ToolDefinition::Function(tool) => Ok(map_function_tool(tool)),
        ToolDefinition::Custom(tool) => Err(invalid_request(format!(
            "Fireworks Chat Completions does not support portable custom tool: {}.",
            tool.name
        ))),
    }
}

fn map_function_tool(tool: &FunctionTool) -> Value {
    let mut function = Map::from_iter([
        ("name".into(), json!(tool.name)),
        ("description".into(), json!(tool.description)),
        ("parameters".into(), json!(tool.parameters)),
    ]);
    if let Some(strict) = tool.strict {
        function.insert("strict".into(), Value::Bool(strict));
    }
    json!({ "type": "function", "function": function })
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
                "Missing tool definition for Fireworks tool call: {name}."
            ))
        })?;
    match tool {
        ToolDefinition::Function(tool) => Ok(tool),
        ToolDefinition::Custom(_) => Err(invalid_request(format!(
            "Fireworks Chat Completions does not support portable custom tool: {name}."
        ))),
    }
}

fn stringify_tool_arguments(arguments: &ToolArguments) -> Result<String, LlmError> {
    match arguments {
        ToolArguments::String(value) => Ok(value.clone()),
        ToolArguments::Object(value) => serde_json::to_string(value).map_err(|error| {
            invalid_request(format!(
                "Could not serialize Fireworks tool arguments: {error}"
            ))
        }),
    }
}
