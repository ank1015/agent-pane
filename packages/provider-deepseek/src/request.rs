use llm_contracts::{
    AssistantContent, ContentPart, CustomMessage, FunctionTool, ImageDetail, ImageSource, LlmError,
    LlmRequest, Message, TextContent, ToolArguments, ToolDefinition, ToolResultMessage,
    ToolResultOutcome, Validate,
};
use serde_json::{Map, Value, json};

use crate::{
    DEEPSEEK_PROVIDER,
    error::{invalid_model, invalid_request},
    find_model,
    models::DeepSeekModel,
};

/// Tag for custom messages containing native DeepSeek Chat Completions turns.
pub const DEEPSEEK_NATIVE_INPUT_TAG: &str = "deepseek.native_input";

/// Builds a non-streaming `POST /chat/completions` JSON body.
pub fn build_chat_completion_request(request: &LlmRequest) -> Result<Value, LlmError> {
    request
        .validate()
        .map_err(|error| invalid_request(error.to_string()))?;
    if request.model.provider.as_str() != DEEPSEEK_PROVIDER {
        return Err(invalid_request(format!(
            "DeepSeek provider cannot handle model provider `{}`.",
            request.model.provider
        )));
    }
    let model = find_model(request.model.id.as_str())
        .ok_or_else(|| invalid_model(request.model.id.as_str()))?;
    build_chat_completion_request_for_model(request, model)
}

pub(crate) fn build_chat_completion_request_for_model(
    request: &LlmRequest,
    model: &DeepSeekModel,
) -> Result<Value, LlmError> {
    let mut body = request.provider_options.clone();
    validate_max_tokens(&body, model)?;
    let native_tools = match body.remove("tools") {
        None => Vec::new(),
        Some(Value::Array(tools)) => tools,
        Some(_) => {
            return Err(invalid_request(
                "DeepSeek provider_options.tools must be an array.",
            ));
        }
    };
    for owned in ["model", "messages", "stream", "stream_options", "n"] {
        body.remove(owned);
    }

    let mut tools = native_tools;
    tools.extend(
        request
            .tools
            .iter()
            .map(map_portable_tool)
            .collect::<Result<Vec<_>, _>>()?,
    );
    if tools.len() > 128 {
        return Err(invalid_request(
            "DeepSeek requests support at most 128 function tools.",
        ));
    }
    let requires_reasoning_content = !tools.is_empty() && thinking_enabled(&body);

    let mut messages = Vec::new();
    if let Some(instructions) = &request.instructions {
        messages.push(json!({ "role": "system", "content": instructions }));
    }
    for message in &request.messages {
        messages.extend(map_message(
            message,
            &request.tools,
            model,
            requires_reasoning_content,
        )?);
    }
    if messages.is_empty() {
        return Err(invalid_request(
            "DeepSeek requests must contain at least one message or instructions.",
        ));
    }

    body.insert("model".into(), Value::String(model.id.to_owned()));
    body.insert("stream".into(), Value::Bool(false));
    body.insert("messages".into(), Value::Array(messages));
    if !tools.is_empty() {
        body.insert("tools".into(), Value::Array(tools));
    }
    Ok(Value::Object(body))
}

fn thinking_enabled(options: &Map<String, Value>) -> bool {
    options
        .get("thinking")
        .and_then(Value::as_object)
        .and_then(|thinking| thinking.get("type"))
        .and_then(Value::as_str)
        != Some("disabled")
}

fn validate_max_tokens(
    options: &Map<String, Value>,
    model: &DeepSeekModel,
) -> Result<(), LlmError> {
    let Some(value) = options.get("max_tokens") else {
        return Ok(());
    };
    let tokens = value
        .as_u64()
        .filter(|tokens| *tokens > 0)
        .ok_or_else(|| invalid_request("DeepSeek max_tokens must be a positive integer."))?;
    if tokens > model.max_tokens {
        return Err(invalid_request(format!(
            "DeepSeek max_tokens cannot exceed {} for {}.",
            model.max_tokens, model.id
        )));
    }
    Ok(())
}

fn map_message(
    message: &Message,
    tools: &[ToolDefinition],
    model: &DeepSeekModel,
    requires_reasoning_content: bool,
) -> Result<Vec<Value>, LlmError> {
    match message {
        Message::User(message) => Ok(vec![json!({
            "role": "user",
            "content": map_user_content(&message.content, model)?,
        })]),
        Message::System(message) => Ok(vec![json!({
            "role": "system",
            "content": join_text(&message.content),
        })]),
        Message::Assistant(message) => {
            map_assistant_message(message, tools, requires_reasoning_content)
        }
        Message::ToolResult(message) => Ok(vec![map_tool_result(message, tools)?]),
        Message::Custom(message) => map_custom_message(message, requires_reasoning_content),
    }
}

fn map_assistant_message(
    message: &llm_contracts::AssistantMessage,
    tools: &[ToolDefinition],
    requires_reasoning_content: bool,
) -> Result<Vec<Value>, LlmError> {
    if message.model.provider.as_str() == DEEPSEEK_PROVIDER {
        let mut native_message = message
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
                    "DeepSeek assistant native_message must contain choice zero with a message object.",
                )
            })?;
        ensure_reasoning_content(&mut native_message, requires_reasoning_content);
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
    native.insert(
        "content".into(),
        if text.is_empty() {
            Value::Null
        } else {
            json!(text.join(""))
        },
    );
    if reasoning.is_empty() {
        if requires_reasoning_content {
            native.insert("reasoning_content".into(), json!(""));
        }
    } else {
        native.insert("reasoning_content".into(), json!(reasoning.join("")));
    }
    if !tool_calls.is_empty() {
        native.insert("tool_calls".into(), Value::Array(tool_calls));
    }
    Ok(vec![Value::Object(native)])
}

fn map_custom_message(
    message: &CustomMessage,
    requires_reasoning_content: bool,
) -> Result<Vec<Value>, LlmError> {
    if message.tag.as_deref() != Some(DEEPSEEK_NATIVE_INPUT_TAG) {
        return Err(invalid_request(format!(
            "Unsupported DeepSeek custom message tag: {}.",
            message.tag.as_deref().unwrap_or("<missing>")
        )));
    }
    let mut messages = message
        .content
        .get("messages")
        .and_then(Value::as_array)
        .cloned()
        .ok_or_else(|| {
            invalid_request("DeepSeek native input content.messages must be an array.")
        })?;
    for message in &mut messages {
        ensure_reasoning_content(message, requires_reasoning_content);
    }
    Ok(messages)
}

fn ensure_reasoning_content(message: &mut Value, required: bool) {
    if !required || message.get("role").and_then(Value::as_str) != Some("assistant") {
        return;
    }
    let object = message
        .as_object_mut()
        .expect("an assistant role can only be read from an object");
    object
        .entry("reasoning_content")
        .or_insert_with(|| json!(""));
}

fn map_tool_result(
    message: &ToolResultMessage,
    tools: &[ToolDefinition],
) -> Result<Value, LlmError> {
    find_function_tool(tools, &message.tool_name)?;
    if message
        .content
        .iter()
        .any(|part| matches!(part, ContentPart::Image(_)))
    {
        return Err(invalid_request(
            "DeepSeek tool results support text content only.",
        ));
    }
    let text = message
        .content
        .iter()
        .filter_map(|part| match part {
            ContentPart::Text(text) => Some(text.content.as_str()),
            ContentPart::Image(_) => None,
        })
        .collect::<Vec<_>>()
        .join("\n");
    let content = if matches!(message.outcome, ToolResultOutcome::Error { .. }) {
        format!("[TOOL ERROR] {text}")
    } else {
        text
    };
    Ok(json!({
        "role": "tool",
        "tool_call_id": message.tool_call_id,
        "content": content,
    }))
}

fn map_user_content(content: &[ContentPart], model: &DeepSeekModel) -> Result<Value, LlmError> {
    if content
        .iter()
        .all(|part| matches!(part, ContentPart::Text(_)))
    {
        return Ok(Value::String(
            content
                .iter()
                .filter_map(|part| match part {
                    ContentPart::Text(text) => Some(text.content.as_str()),
                    ContentPart::Image(_) => None,
                })
                .collect::<Vec<_>>()
                .join("\n"),
        ));
    }
    if !model.supports_images {
        return Err(invalid_request(format!(
            "DeepSeek model {} does not support image input.",
            model.id
        )));
    }
    content
        .iter()
        .map(map_content_part)
        .collect::<Result<Vec<_>, _>>()
        .map(Value::Array)
}

fn map_content_part(part: &ContentPart) -> Result<Value, LlmError> {
    match part {
        ContentPart::Text(text) => Ok(json!({ "type": "text", "text": text.content })),
        ContentPart::Image(image) => {
            let url = match &image.source {
                ImageSource::Base64(source) => {
                    if !matches!(
                        source.mime_type.as_str(),
                        "image/jpeg" | "image/png" | "image/gif" | "image/webp"
                    ) {
                        return Err(invalid_request(format!(
                            "Unsupported DeepSeek image media type: {}.",
                            source.mime_type
                        )));
                    }
                    format!("data:{};base64,{}", source.mime_type, source.data)
                }
                ImageSource::Url(source) => source.url.clone(),
            };
            let mut image_url = Map::from_iter([("url".into(), json!(url))]);
            if let Some(detail) = image.detail {
                image_url.insert("detail".into(), json!(image_detail(detail)));
            }
            Ok(json!({ "type": "image_url", "image_url": image_url }))
        }
    }
}

const fn image_detail(detail: ImageDetail) -> &'static str {
    match detail {
        ImageDetail::Auto => "auto",
        ImageDetail::Low => "low",
        ImageDetail::High => "high",
        ImageDetail::Original => "original",
    }
}

fn join_text(content: &[TextContent]) -> String {
    content
        .iter()
        .map(|part| part.content.as_str())
        .collect::<Vec<_>>()
        .join("\n")
}

fn map_portable_tool(tool: &ToolDefinition) -> Result<Value, LlmError> {
    match tool {
        ToolDefinition::Function(tool) => Ok(map_function_tool(tool)),
        ToolDefinition::Custom(tool) => Err(invalid_request(format!(
            "DeepSeek Chat Completions does not support portable custom tool: {}.",
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
                "Missing tool definition for DeepSeek tool call: {name}."
            ))
        })?;
    match tool {
        ToolDefinition::Function(tool) => Ok(tool),
        ToolDefinition::Custom(_) => Err(invalid_request(format!(
            "DeepSeek Chat Completions does not support portable custom tool: {name}."
        ))),
    }
}

fn stringify_tool_arguments(arguments: &ToolArguments) -> Result<String, LlmError> {
    match arguments {
        ToolArguments::String(value) => Ok(value.clone()),
        ToolArguments::Object(value) => serde_json::to_string(value).map_err(|error| {
            invalid_request(format!(
                "Could not serialize DeepSeek tool arguments: {error}"
            ))
        }),
    }
}
