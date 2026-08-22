use llm_contracts::{
    AssistantContent, AssistantMessage, LlmError, MessageId, ModelId, ModelRef, ProviderId,
    StopReason, TextContent, Timestamp, ToolArguments, ToolCallId, Usage, UsageCost,
};
use serde::Deserialize;
use serde_json::{Map, Value};

use crate::{
    OPENROUTER_PROVIDER,
    error::{invalid_response, normalize_choice_error},
    find_model,
    models::{OpenRouterModel, calculate_usage_cost},
};

#[derive(Debug, Deserialize)]
struct OpenRouterResponse {
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
    #[serde(default)]
    message: Option<Value>,
    #[serde(default)]
    finish_reason: Option<String>,
    #[serde(default)]
    error: Option<Value>,
    #[serde(flatten)]
    _extra: Map<String, Value>,
}

#[derive(Debug, Deserialize)]
struct ResponseUsage {
    prompt_tokens: u64,
    completion_tokens: u64,
    #[serde(default)]
    prompt_tokens_details: PromptTokenDetails,
    #[serde(default)]
    cost: Option<f64>,
    #[serde(default)]
    cost_details: CostDetails,
    #[serde(flatten)]
    _extra: Map<String, Value>,
}

#[derive(Debug, Default, Deserialize)]
struct PromptTokenDetails {
    #[serde(default)]
    cached_tokens: u64,
    #[serde(default)]
    cache_write_tokens: u64,
    #[serde(flatten)]
    _extra: Map<String, Value>,
}

#[derive(Debug, Default, Deserialize)]
struct CostDetails {
    #[serde(default)]
    upstream_inference_cost: Option<f64>,
    #[serde(default)]
    upstream_inference_prompt_cost: Option<f64>,
    #[serde(default, alias = "upstream_inference_completion_cost")]
    upstream_inference_completions_cost: Option<f64>,
    #[serde(flatten)]
    _extra: Map<String, Value>,
}

/// Converts one complete native OpenRouter Chat Completion into the shared contract.
pub fn convert_response(
    native: Value,
    selected_model: &OpenRouterModel,
    duration_ms: u64,
    timestamp_ms: u64,
) -> Result<AssistantMessage, LlmError> {
    let response: OpenRouterResponse = serde_json::from_value(native.clone()).map_err(|error| {
        invalid_response(
            format!("Invalid OpenRouter response: {error}"),
            native.clone(),
        )
    })?;
    if response.object != "chat.completion" {
        return Err(invalid_response(
            "OpenRouter response object must equal `chat.completion`.",
            native,
        ));
    }
    if response.id.trim().is_empty() || response.model.trim().is_empty() {
        return Err(invalid_response(
            "OpenRouter response id and model must not be empty.",
            native,
        ));
    }
    let choice = response
        .choices
        .iter()
        .find(|choice| choice.index == 0)
        .ok_or_else(|| {
            invalid_response(
                "OpenRouter response must contain choice zero.",
                native.clone(),
            )
        })?;
    if let Some(error) = &choice.error {
        return Err(normalize_choice_error(error.clone(), &native));
    }
    let message = choice.message.as_ref().ok_or_else(|| {
        invalid_response(
            "OpenRouter choice zero must contain a message.",
            native.clone(),
        )
    })?;
    let content = content_from_message(message, &native)?;
    let finish_reason = choice.finish_reason.as_deref().ok_or_else(|| {
        invalid_response(
            "OpenRouter choice zero must contain a finish_reason.",
            native.clone(),
        )
    })?;
    let stop_reason = stop_reason(finish_reason, message, &native)?;
    let usage = response
        .usage
        .as_ref()
        .map(|usage| usage_from_response(usage, selected_model, &native))
        .transpose()?;
    let actual_model = find_model(&response.model);

    Ok(AssistantMessage {
        id: MessageId::new(response.id)
            .map_err(|error| invalid_response(error.to_string(), native.clone()))?,
        model: ModelRef {
            provider: ProviderId::new(OPENROUTER_PROVIDER)
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
            "OpenRouter choice zero message must be an object.",
            native.clone(),
        )
    })?;
    if object.get("role").and_then(Value::as_str) != Some("assistant") {
        return Err(invalid_response(
            "OpenRouter choice zero message role must equal `assistant`.",
            native.clone(),
        ));
    }

    let mut content = Vec::new();
    if let Some(reasoning) = optional_string(object.get("reasoning"), "reasoning", native)?
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
    } else if let Some(refusal) = optional_string(object.get("refusal"), "refusal", native)?
        && !refusal.is_empty()
    {
        content.push(text_content(refusal));
    }
    if let Some(tool_calls) = object.get("tool_calls").filter(|value| !value.is_null()) {
        let tool_calls = tool_calls.as_array().ok_or_else(|| {
            invalid_response(
                "OpenRouter message tool_calls must be an array.",
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
            "OpenRouter tool call type must equal `function`.",
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
    model: &OpenRouterModel,
    whole_response: &Value,
) -> Result<Usage, LlmError> {
    let cached = native
        .prompt_tokens_details
        .cached_tokens
        .min(native.prompt_tokens);
    let cache_write = native
        .prompt_tokens_details
        .cache_write_tokens
        .min(native.prompt_tokens.saturating_sub(cached));
    let mut usage = Usage {
        input: Some(native.prompt_tokens.saturating_sub(cached + cache_write)),
        output: Some(native.completion_tokens),
        cache_read: Some(cached),
        cache_write: Some(cache_write),
        cost: None,
    };
    usage.cost = Some(authoritative_or_catalog_cost(
        native,
        &usage,
        model,
        whole_response,
    )?);
    Ok(usage)
}

fn authoritative_or_catalog_cost(
    native: &ResponseUsage,
    usage: &Usage,
    model: &OpenRouterModel,
    whole_response: &Value,
) -> Result<UsageCost, LlmError> {
    for (field, value) in [
        ("usage.cost", native.cost),
        (
            "usage.cost_details.upstream_inference_cost",
            native.cost_details.upstream_inference_cost,
        ),
        (
            "usage.cost_details.upstream_inference_prompt_cost",
            native.cost_details.upstream_inference_prompt_cost,
        ),
        (
            "usage.cost_details.upstream_inference_completions_cost",
            native.cost_details.upstream_inference_completions_cost,
        ),
    ] {
        if value.is_some_and(|value| !value.is_finite() || value < 0.0) {
            return Err(invalid_response(
                format!("OpenRouter response field `{field}` must be finite and non-negative."),
                whole_response.clone(),
            ));
        }
    }

    let details = &native.cost_details;
    let has_authoritative_cost = native.cost.is_some()
        || details.upstream_inference_cost.is_some()
        || details.upstream_inference_prompt_cost.is_some()
        || details.upstream_inference_completions_cost.is_some();
    if !has_authoritative_cost {
        return Ok(calculate_usage_cost(usage, model));
    }
    let input = details.upstream_inference_prompt_cost;
    let output = details.upstream_inference_completions_cost;
    let total = native
        .cost
        .or(details.upstream_inference_cost)
        .unwrap_or_else(|| input.unwrap_or(0.0) + output.unwrap_or(0.0));
    Ok(UsageCost {
        input,
        output,
        cache_read: None,
        cache_write: None,
        total,
    })
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
        "error" => Err(invalid_response(
            "OpenRouter choice ended with `error` but did not include an error object.",
            native.clone(),
        )),
        _ => Err(invalid_response(
            format!("Unsupported OpenRouter finish_reason: {reason}."),
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
            format!("OpenRouter response field `{field}` must be a string or null."),
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
                format!("OpenRouter response field `{field}` must be present."),
                native.clone(),
            )
        })
}

fn required_string<'a>(value: &'a Value, field: &str, native: &Value) -> Result<&'a str, LlmError> {
    required_field(value, field, native)?
        .as_str()
        .ok_or_else(|| {
            invalid_response(
                format!("OpenRouter response field `{field}` must be a string."),
                native.clone(),
            )
        })
}
