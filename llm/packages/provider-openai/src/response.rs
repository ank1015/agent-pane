use llm_contracts::{
    AssistantContent, AssistantMessage, LlmError, MessageId, ModelId, ModelRef, ProviderId,
    StopReason, TextContent, Timestamp, ToolArguments, ToolCallId, Usage,
};
use serde::Deserialize;
use serde_json::{Map, Value};

use crate::{
    OPENAI_PROVIDER,
    error::invalid_response,
    models::{OpenAiModel, calculate_usage_cost},
};

#[derive(Debug, Deserialize)]
struct OpenAiResponse {
    id: String,
    object: String,
    model: String,
    output: Vec<Value>,
    #[serde(default)]
    status: Option<String>,
    #[serde(default)]
    incomplete_details: Option<IncompleteDetails>,
    #[serde(default)]
    error: Option<ResponseError>,
    #[serde(default)]
    usage: Option<ResponseUsage>,
    #[serde(flatten)]
    _extra: Map<String, Value>,
}

#[derive(Debug, Deserialize)]
struct IncompleteDetails {
    #[serde(default)]
    reason: Option<String>,
    #[serde(flatten)]
    _extra: Map<String, Value>,
}

#[derive(Debug, Deserialize)]
struct ResponseError {
    message: String,
    #[serde(default)]
    code: Option<String>,
    #[serde(default, rename = "type")]
    kind: Option<String>,
    #[serde(flatten)]
    _extra: Map<String, Value>,
}

#[derive(Debug, Deserialize)]
struct ResponseUsage {
    input_tokens: u64,
    output_tokens: u64,
    #[serde(default)]
    input_tokens_details: InputTokenDetails,
    #[serde(flatten)]
    _extra: Map<String, Value>,
}

#[derive(Debug, Default, Deserialize)]
struct InputTokenDetails {
    #[serde(default)]
    cached_tokens: u64,
    #[serde(default)]
    cache_write_tokens: u64,
    #[serde(flatten)]
    _extra: Map<String, Value>,
}

/// Converts a complete native OpenAI response into the shared assistant contract.
pub fn convert_response(
    native: Value,
    selected_model: &OpenAiModel,
    duration_ms: u64,
    timestamp_ms: u64,
) -> Result<AssistantMessage, LlmError> {
    let response: OpenAiResponse = serde_json::from_value(native.clone()).map_err(|error| {
        invalid_response(format!("Invalid OpenAI response: {error}"), native.clone())
    })?;
    if response.object != "response" {
        return Err(invalid_response(
            "OpenAI response object must equal `response`.",
            native,
        ));
    }
    if response.id.trim().is_empty() || response.model.trim().is_empty() {
        return Err(invalid_response(
            "OpenAI response id and model must not be empty.",
            native,
        ));
    }
    if response.status.as_deref() == Some("failed") || response.error.is_some() {
        return Err(response_failure(&response, native));
    }

    let content = content_from_response(&response, &native)?;
    let stop_reason = stop_reason_from_response(&response, &content);
    let usage = response
        .usage
        .as_ref()
        .map(|usage| usage_from_response(usage, selected_model));

    Ok(AssistantMessage {
        id: MessageId::new(response.id)
            .map_err(|error| invalid_response(error.to_string(), native.clone()))?,
        model: ModelRef {
            provider: ProviderId::new(OPENAI_PROVIDER)
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

fn content_from_response(
    response: &OpenAiResponse,
    native: &Value,
) -> Result<Vec<AssistantContent>, LlmError> {
    let mut content = Vec::new();
    for output in &response.output {
        let kind = required_string(output, "type", native)?;
        match kind {
            "message" => {
                let parts = required_field(output, "content", native)?
                    .as_array()
                    .ok_or_else(|| {
                        invalid_response("OpenAI message content must be an array.", native.clone())
                    })?;
                for part in parts {
                    let text = match required_string(part, "type", native)? {
                        "output_text" => required_string(part, "text", native)?,
                        "refusal" => required_string(part, "refusal", native)?,
                        _ => continue,
                    };
                    content.push(text_content(text));
                }
            }
            "reasoning" => content.push(AssistantContent::Thinking {
                thinking_text: reasoning_text(output),
            }),
            "function_call" | "custom_tool_call" => {
                content.push(tool_call_from_item(output, native)?);
            }
            _ => {}
        }
    }
    Ok(content)
}

fn usage_from_response(native: &ResponseUsage, model: &OpenAiModel) -> Usage {
    let cached = native.input_tokens_details.cached_tokens;
    let cache_write = native.input_tokens_details.cache_write_tokens;
    let mut usage = Usage {
        input: Some(
            native
                .input_tokens
                .saturating_sub(cached)
                .saturating_sub(cache_write),
        ),
        output: Some(native.output_tokens),
        cache_read: Some(cached),
        cache_write: Some(cache_write),
        cost: None,
    };
    usage.cost = Some(calculate_usage_cost(&usage, model, native.input_tokens));
    usage
}

fn stop_reason_from_response(
    response: &OpenAiResponse,
    content: &[AssistantContent],
) -> StopReason {
    if content
        .iter()
        .any(|part| matches!(part, AssistantContent::ToolCall { .. }))
    {
        return StopReason::ToolUse;
    }
    if response.status.as_deref() == Some("incomplete") {
        return if response
            .incomplete_details
            .as_ref()
            .and_then(|details| details.reason.as_deref())
            == Some("content_filter")
        {
            StopReason::ContentFilter
        } else {
            StopReason::Length
        };
    }
    if response_has_refusal(response) {
        StopReason::Refusal
    } else {
        StopReason::Stop
    }
}

fn response_has_refusal(response: &OpenAiResponse) -> bool {
    response.output.iter().any(|output| {
        output.get("type").and_then(Value::as_str) == Some("message")
            && output
                .get("content")
                .and_then(Value::as_array)
                .is_some_and(|parts| {
                    parts
                        .iter()
                        .any(|part| part.get("type").and_then(Value::as_str) == Some("refusal"))
                })
    })
}

fn response_failure(response: &OpenAiResponse, native: Value) -> LlmError {
    let error = response.error.as_ref();
    LlmError {
        message: error
            .map(|error| error.message.clone())
            .unwrap_or_else(|| "OpenAI response failed.".to_owned()),
        provider_code: error.and_then(|error| error.code.clone()),
        provider_type: error
            .and_then(|error| error.kind.clone())
            .or_else(|| Some("response_failed".to_owned())),
        http_status: None,
        can_retry: false,
        retry_after_ms: None,
        native_error: Some(Box::new(native)),
    }
}

fn tool_call_from_item(item: &Value, native: &Value) -> Result<AssistantContent, LlmError> {
    let kind = required_string(item, "type", native)?;
    let arguments = if kind == "custom_tool_call" {
        ToolArguments::String(required_string(item, "input", native)?.to_owned())
    } else {
        parse_tool_arguments(required_string(item, "arguments", native)?)
    };
    Ok(AssistantContent::ToolCall {
        name: required_string(item, "name", native)?.to_owned(),
        arguments,
        tool_call_id: ToolCallId::new(required_string(item, "call_id", native)?)
            .map_err(|error| invalid_response(error.to_string(), native.clone()))?,
    })
}

fn parse_tool_arguments(input: &str) -> ToolArguments {
    match serde_json::from_str(if input.is_empty() { "{}" } else { input }) {
        Ok(Value::Object(object)) => ToolArguments::Object(object),
        _ => ToolArguments::String(input.to_owned()),
    }
}

fn reasoning_text(item: &Value) -> String {
    let summary = text_array(item.get("summary"));
    if summary.is_empty() {
        text_array(item.get("content"))
    } else {
        summary
    }
}

fn text_array(value: Option<&Value>) -> String {
    value
        .and_then(Value::as_array)
        .map(|parts| {
            parts
                .iter()
                .filter_map(|part| part.get("text").and_then(Value::as_str))
                .collect::<Vec<_>>()
                .join("\n\n")
        })
        .unwrap_or_default()
}

fn text_content(text: &str) -> AssistantContent {
    AssistantContent::Response {
        response: TextContent {
            content: text.to_owned(),
            metadata: None,
        },
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
                format!("OpenAI response field `{field}` must be present."),
                native.clone(),
            )
        })
}

fn required_string<'a>(value: &'a Value, field: &str, native: &Value) -> Result<&'a str, LlmError> {
    required_field(value, field, native)?
        .as_str()
        .ok_or_else(|| {
            invalid_response(
                format!("OpenAI response field `{field}` must be a string."),
                native.clone(),
            )
        })
}
