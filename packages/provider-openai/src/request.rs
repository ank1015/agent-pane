use llm_contracts::{
    AssistantContent, ContentPart, CustomMessage, CustomTool, FunctionTool, GrammarSyntax,
    ImageDetail, ImageSource, LlmError, LlmRequest, Message, TextContent, ToolArguments,
    ToolDefinition, ToolResultMessage, ToolResultOutcome, Validate,
};
use serde_json::{Value, json};

use crate::{
    OPENAI_PROVIDER,
    error::{invalid_model, invalid_request},
    find_model,
    models::OpenAiModel,
};

/// Tag for custom messages containing native OpenAI Responses input items.
pub const OPENAI_NATIVE_INPUT_TAG: &str = "openai.native_input";

/// Builds a non-streaming `POST /responses` JSON body.
pub fn build_response_request(request: &LlmRequest) -> Result<Value, LlmError> {
    request
        .validate()
        .map_err(|error| invalid_request(error.to_string()))?;
    if request.model.provider.as_str() != OPENAI_PROVIDER {
        return Err(invalid_request(format!(
            "OpenAI provider cannot handle model provider `{}`.",
            request.model.provider
        )));
    }
    let model = find_model(request.model.id.as_str())
        .ok_or_else(|| invalid_model(request.model.id.as_str()))?;
    build_response_request_for_model(request, model)
}

pub(crate) fn build_response_request_for_model(
    request: &LlmRequest,
    model: &OpenAiModel,
) -> Result<Value, LlmError> {
    let mut body = request.provider_options.clone();

    for owned in ["model", "input", "instructions", "stream"] {
        body.remove(owned);
    }

    let hosted_tools = match body.remove("tools") {
        None => Vec::new(),
        Some(Value::Array(tools)) => tools,
        Some(_) => {
            return Err(invalid_request(
                "OpenAI provider_options.tools must be an array.",
            ));
        }
    };
    let mut tools = hosted_tools;
    tools.extend(
        request
            .tools
            .iter()
            .map(map_portable_tool)
            .collect::<Result<Vec<_>, _>>()?,
    );

    let mut input = Vec::new();
    for message in &request.messages {
        input.extend(map_message(message, &request.tools)?);
    }

    body.insert("model".into(), Value::String(model.id.to_owned()));
    if let Some(instructions) = &request.instructions {
        body.insert("instructions".into(), Value::String(instructions.clone()));
    }
    if !tools.is_empty() {
        body.insert("tools".into(), Value::Array(tools));
    }
    body.insert("input".into(), Value::Array(input));
    Ok(Value::Object(body))
}

fn map_message(message: &Message, tools: &[ToolDefinition]) -> Result<Vec<Value>, LlmError> {
    match message {
        Message::User(message) => Ok(vec![json!({
            "role": "user",
            "content": map_content(&message.content),
        })]),
        Message::System(message) => Ok(vec![json!({
            "role": "developer",
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
        .map(|part| json!({ "type": "input_text", "text": part.content }))
        .collect()
}

fn map_assistant_message(
    message: &llm_contracts::AssistantMessage,
    tools: &[ToolDefinition],
) -> Result<Vec<Value>, LlmError> {
    if message.model.provider.as_str() == OPENAI_PROVIDER {
        return message
            .native_message
            .as_object()
            .and_then(|response| response.get("output"))
            .and_then(Value::as_array)
            .cloned()
            .ok_or_else(|| {
                invalid_request("OpenAI assistant native_message must contain an output array.")
            });
    }

    let mut input = Vec::new();
    for part in &message.content {
        match part {
            AssistantContent::Response { response } if !response.content.is_empty() => {
                input.push(json!({ "role": "assistant", "content": response.content }));
            }
            AssistantContent::Response { .. } | AssistantContent::Thinking { .. } => {}
            AssistantContent::ToolCall {
                name,
                arguments,
                tool_call_id,
            } => {
                let tool = find_tool(tools, name)?;
                let serialized = stringify_tool_arguments(arguments)?;
                match tool {
                    ToolDefinition::Custom(_) => input.push(json!({
                        "type": "custom_tool_call",
                        "call_id": tool_call_id,
                        "name": name,
                        "input": serialized,
                    })),
                    ToolDefinition::Function(_) => input.push(json!({
                        "type": "function_call",
                        "call_id": tool_call_id,
                        "name": name,
                        "arguments": serialized,
                    })),
                }
            }
        }
    }
    Ok(input)
}

fn map_custom_message(message: &CustomMessage) -> Result<Vec<Value>, LlmError> {
    if message.tag.as_deref() != Some(OPENAI_NATIVE_INPUT_TAG) {
        return Err(invalid_request(format!(
            "Unsupported OpenAI custom message tag: {}.",
            message.tag.as_deref().unwrap_or("<missing>")
        )));
    }
    message
        .content
        .get("items")
        .and_then(Value::as_array)
        .cloned()
        .ok_or_else(|| {
            invalid_request("OpenAI native input message content.items must be an array.")
        })
}

fn map_tool_result(
    message: &ToolResultMessage,
    tools: &[ToolDefinition],
) -> Result<Value, LlmError> {
    let tool = find_tool(tools, &message.tool_name)?;
    let output = if message
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

    let kind = match tool {
        ToolDefinition::Custom(_) => "custom_tool_call_output",
        ToolDefinition::Function(_) => "function_call_output",
    };
    Ok(json!({
        "type": kind,
        "call_id": message.tool_call_id,
        "output": output,
    }))
}

fn find_tool<'a>(tools: &'a [ToolDefinition], name: &str) -> Result<&'a ToolDefinition, LlmError> {
    tools
        .iter()
        .find(|tool| tool.name() == name)
        .ok_or_else(|| {
            invalid_request(format!(
                "Missing tool definition for OpenAI tool call: {name}."
            ))
        })
}

fn map_content(content: &[ContentPart]) -> Vec<Value> {
    content
        .iter()
        .map(|part| match part {
            ContentPart::Text(text) => {
                json!({ "type": "input_text", "text": text.content })
            }
            ContentPart::Image(image) => {
                let image_url = match &image.source {
                    ImageSource::Base64(source) => {
                        format!("data:{};base64,{}", source.mime_type, source.data)
                    }
                    ImageSource::Url(source) => source.url.clone(),
                };
                json!({
                    "type": "input_image",
                    "detail": image.detail.map_or("auto", image_detail),
                    "image_url": image_url,
                })
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
        ToolDefinition::Function(tool) => map_function_tool(tool),
        ToolDefinition::Custom(tool) => Ok(map_custom_tool(tool)),
    }
}

fn map_function_tool(tool: &FunctionTool) -> Result<Value, LlmError> {
    let strict = tool.strict.map_or(Value::Null, Value::Bool);
    Ok(json!({
        "type": "function",
        "name": tool.name,
        "description": tool.description,
        "parameters": tool.parameters,
        "strict": strict,
    }))
}

fn map_custom_tool(tool: &CustomTool) -> Value {
    let syntax = match tool.format.syntax {
        GrammarSyntax::Lark => "lark",
    };
    json!({
        "type": "custom",
        "name": tool.name,
        "description": tool.description,
        "format": {
            "type": "grammar",
            "syntax": syntax,
            "definition": tool.format.definition,
        },
    })
}

fn stringify_tool_arguments(arguments: &ToolArguments) -> Result<String, LlmError> {
    match arguments {
        ToolArguments::String(value) => Ok(value.clone()),
        ToolArguments::Object(value) => serde_json::to_string(value).map_err(|error| {
            invalid_request(format!("Could not serialize tool arguments: {error}"))
        }),
    }
}
