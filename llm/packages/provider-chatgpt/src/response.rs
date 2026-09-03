use llm_contracts::{AssistantMessage, LlmError, ProviderId};
use serde_json::{Map, Value, json};

use crate::{
    CHATGPT_PROVIDER,
    error::{invalid_response, provider_event_error},
    models::ChatGptModel,
};

/// Converts a complete sequence of native ChatGPT SSE event payloads into the
/// shared assistant contract.
pub fn convert_response_events(
    events: Vec<Value>,
    selected_model: &ChatGptModel,
    duration_ms: u64,
    timestamp_ms: u64,
) -> Result<AssistantMessage, LlmError> {
    let terminal = events
        .iter()
        .find(|event| is_terminal_event(event))
        .cloned()
        .ok_or_else(|| {
            invalid_response(
                "ChatGPT stream ended before a terminal response event.",
                native_envelope(None, Vec::new()),
            )
        })?;
    let kind = event_kind(&terminal).unwrap_or_default();
    if matches!(kind, "error" | "response.failed") {
        return Err(provider_event_error(terminal));
    }

    let terminal_response = terminal.get("response").cloned().ok_or_else(|| {
        invalid_response(
            "ChatGPT terminal event must contain a response object.",
            native_envelope(None, Vec::new()),
        )
    })?;
    let mut output = terminal_response
        .get("output")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_else(|| completed_output_items(&events));
    if output.is_empty() {
        output = completed_output_items(&events);
    }
    let native = native_envelope(Some(terminal_response.clone()), output.clone());
    let mut aggregate = terminal_response.clone();
    let response = aggregate.as_object_mut().ok_or_else(|| {
        invalid_response(
            "ChatGPT terminal response must be an object.",
            native.clone(),
        )
    })?;

    response
        .entry("object")
        .or_insert_with(|| Value::String("response".to_owned()));
    response
        .entry("model")
        .or_insert_with(|| Value::String(selected_model.id.to_owned()));
    if !output.is_empty()
        && response
            .get("output")
            .and_then(Value::as_array)
            .is_none_or(Vec::is_empty)
    {
        response.insert("output".into(), Value::Array(output));
    } else {
        response
            .entry("output")
            .or_insert_with(|| Value::Array(output));
    }
    if !response.contains_key("id") {
        if let Some(id) = response_id_from_events(&events) {
            response.insert("id".into(), Value::String(id.to_owned()));
        }
    }
    match kind {
        "response.incomplete" => {
            response.insert("status".into(), Value::String("incomplete".to_owned()));
        }
        "response.done" => {
            response
                .entry("status")
                .or_insert_with(|| Value::String("completed".to_owned()));
        }
        _ => {}
    }

    let mut message =
        provider_openai::convert_response(aggregate, selected_model, duration_ms, timestamp_ms)
            .map_err(|mut error| {
                error.message = error.message.replace("OpenAI", "ChatGPT");
                error.native_error = Some(Box::new(native.clone()));
                error
            })?;
    message.model.provider =
        ProviderId::new(CHATGPT_PROVIDER).expect("the built-in ChatGPT provider id is valid");
    message.native_message = native;
    Ok(message)
}

pub(crate) fn is_terminal_event(event: &Value) -> bool {
    matches!(
        event_kind(event),
        Some(
            "response.completed"
                | "response.incomplete"
                | "response.done"
                | "response.failed"
                | "error"
        )
    )
}

pub(crate) fn drain_sse_events(
    buffer: &mut Vec<u8>,
    end_of_stream: bool,
) -> Result<Vec<Value>, LlmError> {
    let mut events = Vec::new();
    while let Some((boundary, delimiter_len)) = find_event_boundary(buffer) {
        let bytes = buffer.drain(..boundary).collect::<Vec<_>>();
        buffer.drain(..delimiter_len);
        if let Some(event) = parse_sse_block(&bytes)? {
            events.push(event);
        }
    }
    if end_of_stream && !buffer.is_empty() {
        let bytes = std::mem::take(buffer);
        if let Some(event) = parse_sse_block(&bytes)? {
            events.push(event);
        }
    }
    Ok(events)
}

fn event_kind(event: &Value) -> Option<&str> {
    event.get("type").and_then(Value::as_str)
}

fn completed_output_items(events: &[Value]) -> Vec<Value> {
    events
        .iter()
        .filter(|event| event_kind(event) == Some("response.output_item.done"))
        .filter_map(|event| event.get("item").cloned())
        .collect()
}

fn response_id_from_events(events: &[Value]) -> Option<&str> {
    events.iter().find_map(|event| {
        event
            .get("response")
            .and_then(|response| response.get("id"))
            .and_then(Value::as_str)
    })
}

fn native_envelope(response: Option<Value>, output: Vec<Value>) -> Value {
    let mut native = Map::from_iter([
        (
            "type".to_owned(),
            Value::String("chatgpt_response_stream".to_owned()),
        ),
        ("output".to_owned(), Value::Array(output)),
    ]);
    if let Some(response) = response {
        native.insert("response".to_owned(), response);
    }
    Value::Object(native)
}

fn find_event_boundary(buffer: &[u8]) -> Option<(usize, usize)> {
    let lf = find_bytes(buffer, b"\n\n").map(|index| (index, 2));
    let crlf = find_bytes(buffer, b"\r\n\r\n").map(|index| (index, 4));
    match (lf, crlf) {
        (Some(left), Some(right)) => Some(if left.0 <= right.0 { left } else { right }),
        (Some(found), None) | (None, Some(found)) => Some(found),
        (None, None) => None,
    }
}

fn find_bytes(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    haystack
        .windows(needle.len())
        .position(|window| window == needle)
}

fn parse_sse_block(bytes: &[u8]) -> Result<Option<Value>, LlmError> {
    let block = std::str::from_utf8(bytes).map_err(|error| {
        invalid_response(
            format!("ChatGPT returned invalid UTF-8 SSE data: {error}"),
            Value::String(String::from_utf8_lossy(bytes).into_owned()),
        )
    })?;
    let data = block
        .lines()
        .filter_map(|line| {
            let line = line.strip_suffix('\r').unwrap_or(line);
            line.strip_prefix("data:")
                .map(|value| value.strip_prefix(' ').unwrap_or(value))
        })
        .collect::<Vec<_>>()
        .join("\n");
    if data.trim().is_empty() || data.trim() == "[DONE]" {
        return Ok(None);
    }
    serde_json::from_str(&data).map(Some).map_err(|error| {
        invalid_response(
            format!("ChatGPT returned invalid SSE JSON: {error}"),
            json!({ "data": data }),
        )
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn decoder_handles_crlf_and_split_buffers() {
        let mut buffer = b"data: {\"type\":\"response.created\"}\r\n\r\ndata: {\"type\":".to_vec();
        let first = drain_sse_events(&mut buffer, false).expect("valid first event");
        assert_eq!(first.len(), 1);
        buffer.extend_from_slice(b"\"response.completed\",\"response\":{\"id\":\"r\"}}\n\n");
        let second = drain_sse_events(&mut buffer, false).expect("valid second event");
        assert_eq!(second[0]["response"]["id"], "r");
    }
}
