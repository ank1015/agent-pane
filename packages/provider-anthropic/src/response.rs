use llm_contracts::{
    AssistantContent, AssistantMessage, LlmError, MessageId, ModelId, ModelRef, ProviderId,
    StopReason, TextContent, Timestamp, ToolArguments, ToolCallId, Usage,
};
use serde::Deserialize;
use serde_json::{Map, Value};

use crate::{
    ANTHROPIC_PROVIDER,
    error::invalid_response,
    models::{AnthropicModel, calculate_usage_cost_with_cache_ttl},
};

#[derive(Debug, Deserialize)]
struct AnthropicResponse {
    id: String,
    #[serde(rename = "type")]
    kind: String,
    role: String,
    model: String,
    content: Vec<Value>,
    stop_reason: String,
    usage: ResponseUsage,
    #[serde(flatten)]
    _extra: Map<String, Value>,
}

#[derive(Debug, Deserialize)]
struct ResponseUsage {
    input_tokens: u64,
    output_tokens: u64,
    #[serde(default)]
    cache_read_input_tokens: u64,
    #[serde(default)]
    cache_creation_input_tokens: u64,
    #[serde(default)]
    cache_creation: Option<CacheCreation>,
    #[serde(flatten)]
    _extra: Map<String, Value>,
}

#[derive(Debug, Default, Deserialize)]
struct CacheCreation {
    #[serde(default)]
    ephemeral_5m_input_tokens: u64,
    #[serde(default)]
    ephemeral_1h_input_tokens: u64,
    #[serde(flatten)]
    _extra: Map<String, Value>,
}

/// Converts one complete native Anthropic Message into the shared contract.
pub fn convert_response(
    native: Value,
    selected_model: &AnthropicModel,
    duration_ms: u64,
    timestamp_ms: u64,
) -> Result<AssistantMessage, LlmError> {
    let response: AnthropicResponse = serde_json::from_value(native.clone()).map_err(|error| {
        invalid_response(
            format!("Invalid Anthropic response: {error}"),
            native.clone(),
        )
    })?;
    if response.kind != "message" {
        return Err(invalid_response(
            "Anthropic response type must equal `message`.",
            native,
        ));
    }
    if response.role != "assistant" {
        return Err(invalid_response(
            "Anthropic response role must equal `assistant`.",
            native,
        ));
    }
    if response.id.trim().is_empty() || response.model.trim().is_empty() {
        return Err(invalid_response(
            "Anthropic response id and model must not be empty.",
            native,
        ));
    }

    let content = content_from_blocks(&response.content, &native)?;
    let stop_reason = stop_reason(&response.stop_reason, &native)?;
    let usage = usage_from_response(&response.usage, selected_model);

    Ok(AssistantMessage {
        id: MessageId::new(response.id)
            .map_err(|error| invalid_response(error.to_string(), native.clone()))?,
        model: ModelRef {
            provider: ProviderId::new(ANTHROPIC_PROVIDER)
                .map_err(|error| invalid_response(error.to_string(), native.clone()))?,
            id: ModelId::new(selected_model.id)
                .map_err(|error| invalid_response(error.to_string(), native.clone()))?,
            name: Some(selected_model.name.to_owned()),
        },
        usage: Some(usage),
        duration_ms,
        native_message: native,
        content,
        stop_reason,
        timestamp: Timestamp(timestamp_ms),
    })
}

fn content_from_blocks(
    blocks: &[Value],
    native: &Value,
) -> Result<Vec<AssistantContent>, LlmError> {
    let mut content = Vec::new();
    for block in blocks {
        match required_string(block, "type", native)? {
            "text" => {
                let text = required_string(block, "text", native)?;
                if !text.is_empty() {
                    content.push(AssistantContent::Response {
                        response: TextContent {
                            content: text.to_owned(),
                            metadata: None,
                        },
                    });
                }
            }
            "thinking" => {
                let thinking = required_string(block, "thinking", native)?;
                if !thinking.is_empty() {
                    content.push(AssistantContent::Thinking {
                        thinking_text: thinking.to_owned(),
                    });
                }
            }
            "redacted_thinking" => content.push(AssistantContent::Thinking {
                thinking_text: "[Reasoning redacted by Anthropic]".to_owned(),
            }),
            "tool_use" => content.push(tool_call(block, native)?),
            // Hosted-tool and evolving provider-native blocks remain available in
            // native_message and are replayed unmodified on Anthropic follow-ups.
            _ => {}
        }
    }
    Ok(content)
}

fn tool_call(block: &Value, native: &Value) -> Result<AssistantContent, LlmError> {
    let input = required_field(block, "input", native)?;
    let arguments = match input {
        Value::Object(object) => ToolArguments::Object(object.clone()),
        Value::String(value) => ToolArguments::String(value.clone()),
        value => ToolArguments::String(value.to_string()),
    };
    Ok(AssistantContent::ToolCall {
        name: required_string(block, "name", native)?.to_owned(),
        arguments,
        tool_call_id: ToolCallId::new(required_string(block, "id", native)?)
            .map_err(|error| invalid_response(error.to_string(), native.clone()))?,
    })
}

fn usage_from_response(native: &ResponseUsage, model: &AnthropicModel) -> Usage {
    let (explicit_5m, explicit_1h) = native
        .cache_creation
        .as_ref()
        .map(|cache| {
            (
                cache.ephemeral_5m_input_tokens,
                cache.ephemeral_1h_input_tokens,
            )
        })
        .unwrap_or_default();
    let accounted = explicit_5m.saturating_add(explicit_1h);
    let cache_write_5m =
        explicit_5m.saturating_add(native.cache_creation_input_tokens.saturating_sub(accounted));
    let mut usage = Usage {
        input: Some(native.input_tokens),
        output: Some(native.output_tokens),
        cache_read: Some(native.cache_read_input_tokens),
        cache_write: Some(native.cache_creation_input_tokens),
        cost: None,
    };
    usage.cost = Some(calculate_usage_cost_with_cache_ttl(
        &usage,
        model,
        cache_write_5m,
        explicit_1h.min(native.cache_creation_input_tokens),
    ));
    usage
}

fn stop_reason(reason: &str, native: &Value) -> Result<StopReason, LlmError> {
    match reason {
        "end_turn" | "stop_sequence" => Ok(StopReason::Stop),
        "max_tokens" | "model_context_window_exceeded" => Ok(StopReason::Length),
        "tool_use" => Ok(StopReason::ToolUse),
        "refusal" => Ok(StopReason::Refusal),
        "pause_turn" | "compaction" => Ok(StopReason::PauseTurn),
        _ => Err(invalid_response(
            format!("Unsupported Anthropic stop_reason: {reason}."),
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
                format!("Anthropic response field `{field}` must be present."),
                native.clone(),
            )
        })
}

fn required_string<'a>(value: &'a Value, field: &str, native: &Value) -> Result<&'a str, LlmError> {
    required_field(value, field, native)?
        .as_str()
        .ok_or_else(|| {
            invalid_response(
                format!("Anthropic response field `{field}` must be a string."),
                native.clone(),
            )
        })
}
