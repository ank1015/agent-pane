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
        format!("Unsupported Anthropic model: {model_id}."),
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
    let parsed = if body.is_empty() {
        None
    } else {
        Some(serde_json::from_str(body).unwrap_or_else(|_| Value::String(body.to_owned())))
    };
    let inner = parsed
        .as_ref()
        .and_then(|value| value.get("error"))
        .filter(|value| value.is_object());
    let message = inner
        .and_then(|value| value.get("message"))
        .and_then(Value::as_str)
        .map(ToOwned::to_owned)
        .or_else(|| (!body.is_empty()).then(|| body.to_owned()))
        .or_else(|| (!status_text.is_empty()).then(|| status_text.to_owned()))
        .unwrap_or_else(|| format!("Anthropic request failed with HTTP {status}."));
    let provider_type = inner
        .and_then(|value| value.get("type"))
        .and_then(Value::as_str)
        .map(ToOwned::to_owned);

    LlmError {
        message,
        provider_code: parsed
            .as_ref()
            .and_then(|value| value.get("request_id"))
            .and_then(Value::as_str)
            .map(ToOwned::to_owned),
        provider_type,
        http_status: Some(status),
        can_retry: matches!(status, 408 | 409 | 429) || status >= 500,
        retry_after_ms: retry_after.and_then(|value| parse_retry_after(value, now)),
        native_error: parsed.map(Box::new),
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
