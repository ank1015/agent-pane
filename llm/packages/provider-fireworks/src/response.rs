use llm_contracts::{
    AssistantContent, AssistantMessage, LlmError, MessageId, ModelId, ModelRef, ProviderId,
    StopReason, TextContent, Timestamp, ToolArguments, ToolCallId, Usage,
};
use serde::Deserialize;
use serde_json::{Map, Value};

use crate::{
    FIREWORKS_PROVIDER,
    error::invalid_response,
    models::{FireworksModel, calculate_usage_cost},
};

#[derive(Debug, Deserialize)]
struct FireworksResponse {
    id: String,
    object: String,
    model: String,
    choices: Vec<Choice>,
    #[serde(default)]
    usage: Option<ResponseUsage>,
    #[serde(flatten)]
    _extra: Map<String, Value>,
}

#[derive(Debug, Deserialize)]
struct Choice {
    index: u64,
    message: Value,
    #[serde(default)]
    finish_reason: Option<String>,
    #[serde(flatten)]
    _extra: Map<String, Value>,
}

#[derive(Debug, Deserialize)]
struct ResponseUsage {
    prompt_tokens: u64,
    completion_tokens: u64,
    #[serde(default)]
    prompt_tokens_details: PromptTokenDetails,
    #[serde(flatten)]
    _extra: Map<String, Value>,
}

#[derive(Debug, Default, Deserialize)]
struct PromptTokenDetails {
    #[serde(default)]
    cached_tokens: u64,
    #[serde(flatten)]
    _extra: Map<String, Value>,
}

/// Converts one complete native Fireworks Chat Completion into the shared contract.
pub fn convert_response(
    native: Value,
    selected_model: &FireworksModel,
    duration_ms: u64,
    timestamp_ms: u64,
) -> Result<AssistantMessage, LlmError> {
    let response: FireworksResponse = serde_json::from_value(native.clone()).map_err(|error| {
        invalid_response(
            format!("Invalid Fireworks response: {error}"),
            native.clone(),
        )
    })?;
    if response.object != "chat.completion" {
        return Err(invalid_response(
            "Fireworks response object must equal `chat.completion`.",
            native,
        ));
    }
    if response.id.trim().is_empty() || response.model.trim().is_empty() {
        return Err(invalid_response(
            "Fireworks response id and model must not be empty.",
            native,
        ));
    }
    let choice = response
        .choices
        .iter()
        .find(|choice| choice.index == 0)
        .ok_or_else(|| {
            invalid_response(
                "Fireworks response must contain choice zero.",
                native.clone(),
            )
        })?;
    let content = content_from_message(&choice.message, &native)?;
    let finish_reason = choice.finish_reason.as_deref().ok_or_else(|| {
        invalid_response(
            "Fireworks choice zero must contain a finish_reason.",
            native.clone(),
        )
    })?;
    let stop_reason = stop_reason(finish_reason, &choice.message, &native)?;
    let usage = response
        .usage
        .as_ref()
        .map(|usage| usage_from_response(usage, selected_model));

    Ok(AssistantMessage {
        id: MessageId::new(response.id)
            .map_err(|error| invalid_response(error.to_string(), native.clone()))?,
        model: ModelRef {
            provider: ProviderId::new(FIREWORKS_PROVIDER)
                .map_err(|error| invalid_response(error.to_string(), native.clone()))?,
            id: ModelId::new(selected_model.id)
                .map_err(|error| invalid_response(error.to_string(), native.clone()))?,
            name: Some(selected_model.name.to_owned()),
        },
        usage,
        duration_ms,
        native_message: native,
        content,
        stop_reason,
        timestamp: Timestamp(timestamp_ms),
    })
}

fn content_from_message(
    message: &Value,
    native: &Value,
) -> Result<Vec<AssistantContent>, LlmError> {
    let object = message.as_object().ok_or_else(|| {
        invalid_response(
            "Fireworks choice zero message must be an object.",
            native.clone(),
        )
    })?;
    if object.get("role").and_then(Value::as_str) != Some("assistant") {
        return Err(invalid_response(
            "Fireworks choice zero message role must equal `assistant`.",
            native.clone(),
        ));
    }

    let mut content = Vec::new();
    if let Some(reasoning) =
        optional_string(object.get("reasoning_content"), "reasoning_content", native)?
    {
        if !reasoning.is_empty() {
            content.push(AssistantContent::Thinking {
                thinking_text: reasoning.to_owned(),
            });
        }
    }
    if let Some(text) =
        optional_string(object.get("content"), "content", native)?.filter(|text| !text.is_empty())
    {
        content.push(text_content(text));
    } else if let Some(refusal) =
        optional_string(object.get("refusal"), "refusal", native)?.filter(|text| !text.is_empty())
    {
        content.push(text_content(refusal));
    }
    if let Some(tool_calls) = object.get("tool_calls").filter(|value| !value.is_null()) {
        let tool_calls = tool_calls.as_array().ok_or_else(|| {
            invalid_response(
                "Fireworks message tool_calls must be an array.",
                native.clone(),
            )
        })?;
        for call in tool_calls {
            content.push(tool_call(call, native)?);
        }
    }
    Ok(content)
}

fn tool_call(call: &Value, native: &Value) -> Result<AssistantContent, LlmError> {
    if required_string(call, "type", native)? != "function" {
        return Err(invalid_response(
            "Fireworks tool call type must equal `function`.",
            native.clone(),
        ));
    }
    let function = required_field(call, "function", native)?;
    let arguments = parse_tool_arguments(required_string(function, "arguments", native)?);
    Ok(AssistantContent::ToolCall {
        name: required_string(function, "name", native)?.to_owned(),
        arguments,
        tool_call_id: ToolCallId::new(required_string(call, "id", native)?)
            .map_err(|error| invalid_response(error.to_string(), native.clone()))?,
    })
}

fn parse_tool_arguments(input: &str) -> ToolArguments {
    match serde_json::from_str(if input.is_empty() { "{}" } else { input }) {
        Ok(Value::Object(object)) => ToolArguments::Object(object),
        _ => ToolArguments::String(input.to_owned()),
    }
}

fn usage_from_response(native: &ResponseUsage, model: &FireworksModel) -> Usage {
    let cached = native
        .prompt_tokens_details
        .cached_tokens
        .min(native.prompt_tokens);
    let mut usage = Usage {
        input: Some(native.prompt_tokens.saturating_sub(cached)),
        output: Some(native.completion_tokens),
        cache_read: Some(cached),
        cache_write: None,
        cost: None,
    };
    usage.cost = Some(calculate_usage_cost(&usage, model));
    usage
}

fn stop_reason(reason: &str, message: &Value, native: &Value) -> Result<StopReason, LlmError> {
    match reason {
        "stop" if message.get("refusal").is_some_and(|value| !value.is_null()) => {
            Ok(StopReason::Refusal)
        }
        "stop" => Ok(StopReason::Stop),
        "length" => Ok(StopReason::Length),
        "tool_calls" | "function_call" => Ok(StopReason::ToolUse),
        "content_filter" => Ok(StopReason::ContentFilter),
        _ => Err(invalid_response(
            format!("Unsupported Fireworks finish_reason: {reason}."),
            native.clone(),
        )),
    }
}

fn text_content(text: &str) -> AssistantContent {
    AssistantContent::Response {
        response: TextContent {
            content: text.to_owned(),
            metadata: None,
        },
    }
}

fn optional_string<'a>(
    value: Option<&'a Value>,
    field: &str,
    native: &Value,
) -> Result<Option<&'a str>, LlmError> {
    match value {
        None | Some(Value::Null) => Ok(None),
        Some(Value::String(value)) => Ok(Some(value)),
        Some(_) => Err(invalid_response(
            format!("Fireworks response field `{field}` must be a string or null."),
            native.clone(),
        )),
    }
}

fn required_field<'a>(
    value: &'a Value,
    field: &str,
    native: &Value,
) -> Result<&'a Value, LlmError> {
    value
        .as_object()
        .and_then(|object| object.get(field))
        .ok_or_else(|| {
            invalid_response(
                format!("Fireworks response field `{field}` must be present."),
                native.clone(),
            )
        })
}

fn required_string<'a>(value: &'a Value, field: &str, native: &Value) -> Result<&'a str, LlmError> {
    required_field(value, field, native)?
        .as_str()
        .ok_or_else(|| {
            invalid_response(
                format!("Fireworks response field `{field}` must be a string."),
                native.clone(),
            )
        })
}
