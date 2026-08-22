use std::time::{Duration, SystemTime, UNIX_EPOCH};

use llm_contracts::LlmError;
use serde_json::Value;

pub(crate) fn invalid_config(message: impl Into<String>) -> LlmError {
    error(message, "invalid_config", false, None)
}

pub(crate) fn invalid_request(message: impl Into<String>) -> LlmError {
    error(message, "invalid_request", false, None)
}

pub(crate) fn invalid_model(model_id: &str) -> LlmError {
    error(
        format!("Unsupported OpenRouter model: {model_id}."),
        "invalid_model",
        false,
        None,
    )
}

pub(crate) fn invalid_response(message: impl Into<String>, native: Value) -> LlmError {
    error(message, "invalid_response", false, Some(native))
}

pub(crate) fn network_error(error: &reqwest::Error) -> LlmError {
    let provider_type = if error.is_timeout() {
        "timeout"
    } else if error.is_connect() {
        "connection_error"
    } else {
        "network_error"
    };
    LlmError {
        message: error.to_string(),
        provider_code: None,
        provider_type: Some(provider_type.to_owned()),
        http_status: error.status().map(|status| status.as_u16()),
        can_retry: error.is_timeout() || error.is_connect(),
        retry_after_ms: None,
        native_error: None,
    }
}

pub(crate) fn normalize_http_error(
    status: u16,
    status_text: &str,
    body: &str,
    retry_after: Option<&str>,
    now: SystemTime,
) -> LlmError {
    let parsed = parse_body(body);
    let native_error = parsed
        .as_ref()
        .and_then(|value| value.get("error"))
        .cloned()
        .or_else(|| parsed.clone());
    provider_error(
        native_error,
        Some(status),
        status_text,
        body,
        retry_after.and_then(|value| parse_retry_after(value, now)),
    )
}

pub(crate) fn normalize_choice_error(native_error: Value, whole_response: &Value) -> LlmError {
    let mut normalized = provider_error(Some(native_error), Some(200), "", "", None);
    normalized.native_error = Some(Box::new(whole_response.clone()));
    normalized
}

fn provider_error(
    native_error: Option<Value>,
    status: Option<u16>,
    status_text: &str,
    raw_body: &str,
    retry_after_ms: Option<u64>,
) -> LlmError {
    let message = string_field(native_error.as_ref(), "message")
        .map(ToOwned::to_owned)
        .or_else(|| (!raw_body.is_empty()).then(|| raw_body.to_owned()))
        .or_else(|| (!status_text.is_empty()).then(|| status_text.to_owned()))
        .unwrap_or_else(|| "OpenRouter request failed.".to_owned());
    let metadata = native_error
        .as_ref()
        .and_then(|value| value.get("metadata"));
    let provider_type = string_field(metadata, "error_type")
        .or_else(|| string_field(native_error.as_ref(), "type"))
        .unwrap_or("openrouter_error")
        .to_owned();
    let provider_code = field_as_string(metadata, "provider_code")
        .or_else(|| field_as_string(native_error.as_ref(), "code"));
    let status_code = status.unwrap_or_default();

    LlmError {
        message,
        provider_code,
        provider_type: Some(provider_type),
        http_status: status,
        can_retry: matches!(status_code, 408 | 429 | 524 | 529) || status_code >= 500,
        retry_after_ms,
        native_error: native_error.map(Box::new),
    }
}

fn parse_body(body: &str) -> Option<Value> {
    if body.is_empty() {
        None
    } else {
        Some(serde_json::from_str(body).unwrap_or_else(|_| Value::String(body.to_owned())))
    }
}

fn string_field<'a>(value: Option<&'a Value>, field: &str) -> Option<&'a str> {
    value?.as_object()?.get(field)?.as_str()
}

fn field_as_string(value: Option<&Value>, field: &str) -> Option<String> {
    match value?.as_object()?.get(field)? {
        Value::String(value) => Some(value.clone()),
        Value::Number(value) => Some(value.to_string()),
        _ => None,
    }
}

fn parse_retry_after(value: &str, now: SystemTime) -> Option<u64> {
    if let Ok(seconds) = value.parse::<f64>()
        && seconds.is_finite()
        && seconds >= 0.0
    {
        return Some((seconds * 1_000.0).round() as u64);
    }
    let date = httpdate::parse_http_date(value).ok()?;
    let delay = date.duration_since(now).unwrap_or(Duration::ZERO);
    delay.as_millis().try_into().ok()
}

pub(crate) fn unix_millis(time: SystemTime) -> u64 {
    time.duration_since(UNIX_EPOCH)
        .unwrap_or(Duration::ZERO)
        .as_millis()
        .try_into()
        .unwrap_or(u64::MAX)
}

fn error(
    message: impl Into<String>,
    provider_type: &str,
    can_retry: bool,
    native_error: Option<Value>,
) -> LlmError {
    LlmError {
        message: message.into(),
        provider_code: None,
        provider_type: Some(provider_type.to_owned()),
        http_status: None,
        can_retry,
        retry_after_ms: None,
        native_error: native_error.map(Box::new),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn normalizes_openrouter_metadata_and_retry_after() {
        let error = normalize_http_error(
            429,
            "Too Many Requests",
            r#"{"error":{"code":429,"message":"slow down","metadata":{"error_type":"rate_limit_error","provider_code":"upstream-429"}}}"#,
            Some("1.5"),
            UNIX_EPOCH,
        );
        assert_eq!(error.message, "slow down");
        assert_eq!(error.provider_type.as_deref(), Some("rate_limit_error"));
        assert_eq!(error.provider_code.as_deref(), Some("upstream-429"));
        assert_eq!(error.retry_after_ms, Some(1_500));
        assert!(error.can_retry);
    }
}
