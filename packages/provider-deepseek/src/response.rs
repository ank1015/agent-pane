use llm_contracts::{
    AssistantContent, AssistantMessage, LlmError, MessageId, ModelId, ModelRef, ProviderId,
    StopReason, TextContent, Timestamp, ToolArguments, ToolCallId, Usage,
};
use serde::Deserialize;
use serde_json::{Map, Value};

use crate::{
    DEEPSEEK_PROVIDER,
    error::{insufficient_resource, invalid_response},
    find_model,
    models::{DeepSeekModel, calculate_usage_cost},
};

#[derive(Debug, Deserialize)]
struct DeepSeekResponse {
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
    finish_reason: String,
    #[serde(flatten)]
    _extra: Map<String, Value>,
}

#[derive(Debug, Deserialize)]
struct ResponseUsage {
    prompt_tokens: u64,
    completion_tokens: u64,
    #[serde(default)]
    prompt_cache_hit_tokens: Option<u64>,
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

/// Converts one complete native DeepSeek Chat Completion into the shared contract.
pub fn convert_response(
    native: Value,
    selected_model: &DeepSeekModel,
    duration_ms: u64,
    timestamp_ms: u64,
) -> Result<AssistantMessage, LlmError> {
    let response: DeepSeekResponse = serde_json::from_value(native.clone()).map_err(|error| {
        invalid_response(
            format!("Invalid DeepSeek response: {error}"),
            native.clone(),
        )
    })?;
    if response.object != "chat.completion" {
        return Err(invalid_response(
            "DeepSeek response object must equal `chat.completion`.",
            native,
        ));
    }
    if response.id.trim().is_empty() || response.model.trim().is_empty() {
        return Err(invalid_response(
            "DeepSeek response id and model must not be empty.",
            native,
        ));
    }
    let choice = response
        .choices
        .iter()
        .find(|choice| choice.index == 0)
        .ok_or_else(|| {
            invalid_response(
                "DeepSeek response must contain choice zero.",
                native.clone(),
            )
        })?;
    if choice.finish_reason == "insufficient_system_resource" {
        return Err(insufficient_resource(native));
    }
    let content = content_from_message(&choice.message, &native)?;
    let stop_reason = stop_reason(&choice.finish_reason, &native)?;
    let pricing_timestamp_ms = timestamp_ms.saturating_sub(duration_ms);
    let usage = response
        .usage
        .as_ref()
        .map(|usage| usage_from_response(usage, selected_model, pricing_timestamp_ms));
    let actual_model = find_model(&response.model);

    Ok(AssistantMessage {
        id: MessageId::new(response.id)
            .map_err(|error| invalid_response(error.to_string(), native.clone()))?,
        model: ModelRef {
            provider: ProviderId::new(DEEPSEEK_PROVIDER)
                .map_err(|error| invalid_response(error.to_string(), native.clone()))?,
            id: ModelId::new(response.model)
                .map_err(|error| invalid_response(error.to_string(), native.clone()))?,
            name: actual_model.map(|model| model.name.to_owned()),
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
            "DeepSeek choice zero message must be an object.",
            native.clone(),
        )
    })?;
    if object.get("role").and_then(Value::as_str) != Some("assistant") {
        return Err(invalid_response(
            "DeepSeek choice zero message role must equal `assistant`.",
            native.clone(),
        ));
    }

    let mut content = Vec::new();
    if let Some(reasoning) =
        optional_string(object.get("reasoning_content"), "reasoning_content", native)?
        && !reasoning.is_empty()
    {
        content.push(AssistantContent::Thinking {
            thinking_text: reasoning.to_owned(),
        });
    }
    if let Some(text) = optional_string(object.get("content"), "content", native)?
        && !text.is_empty()
    {
        content.push(text_content(text));
    }
    if let Some(tool_calls) = object.get("tool_calls").filter(|value| !value.is_null()) {
        let tool_calls = tool_calls.as_array().ok_or_else(|| {
            invalid_response(
                "DeepSeek message tool_calls must be an array.",
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
            "DeepSeek tool call type must equal `function`.",
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

fn usage_from_response(
    native: &ResponseUsage,
    model: &DeepSeekModel,
    pricing_timestamp_ms: u64,
) -> Usage {
    let cached = native
        .prompt_cache_hit_tokens
        .unwrap_or(native.prompt_tokens_details.cached_tokens)
        .min(native.prompt_tokens);
    let mut usage = Usage {
        input: Some(native.prompt_tokens.saturating_sub(cached)),
        output: Some(native.completion_tokens),
        cache_read: Some(cached),
        cache_write: None,
        cost: None,
    };
    usage.cost = Some(calculate_usage_cost(&usage, model, pricing_timestamp_ms));
    usage
}

fn stop_reason(reason: &str, native: &Value) -> Result<StopReason, LlmError> {
    match reason {
        "stop" => Ok(StopReason::Stop),
        "length" => Ok(StopReason::Length),
        "tool_calls" | "function_call" => Ok(StopReason::ToolUse),
        "content_filter" => Ok(StopReason::ContentFilter),
        _ => Err(invalid_response(
            format!("Unsupported DeepSeek finish_reason: {reason}."),
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
            format!("DeepSeek response field `{field}` must be a string or null."),
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
                format!("DeepSeek response field `{field}` must be present."),
                native.clone(),
            )
        })
}

fn required_string<'a>(value: &'a Value, field: &str, native: &Value) -> Result<&'a str, LlmError> {
    required_field(value, field, native)?
        .as_str()
        .ok_or_else(|| {
            invalid_response(
                format!("DeepSeek response field `{field}` must be a string."),
                native.clone(),
            )
        })
}
