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
        format!("Unsupported DeepSeek model: {model_id}."),
        "invalid_model",
        false,
        None,
    )
}

pub(crate) fn invalid_response(message: impl Into<String>, native: Value) -> LlmError {
    error(message, "invalid_response", false, Some(native))
}

pub(crate) fn insufficient_resource(native: Value) -> LlmError {
    LlmError {
        message: "DeepSeek generation was interrupted due to insufficient inference resources."
            .to_owned(),
        provider_code: None,
        provider_type: Some("insufficient_system_resource".to_owned()),
        http_status: Some(200),
        can_retry: true,
        retry_after_ms: None,
        native_error: Some(Box::new(native)),
    }
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
    let parsed = if body.trim().is_empty() {
        None
    } else {
        Some(serde_json::from_str(body).unwrap_or_else(|_| Value::String(body.to_owned())))
    };
    let native_error = parsed
        .as_ref()
        .and_then(|value| value.get("error"))
        .cloned()
        .or_else(|| parsed.clone());
    let message = string_field(native_error.as_ref(), "message")
        .map(ToOwned::to_owned)
        .or_else(|| (!body.trim().is_empty()).then(|| body.trim().to_owned()))
        .or_else(|| (!status_text.is_empty()).then(|| status_text.to_owned()))
        .unwrap_or_else(|| format!("DeepSeek request failed with HTTP {status}."));

    LlmError {
        message,
        provider_code: field_as_string(native_error.as_ref(), "code"),
        provider_type: string_field(native_error.as_ref(), "type").map(ToOwned::to_owned),
        http_status: Some(status),
        can_retry: matches!(status, 408 | 429) || status >= 500,
        retry_after_ms: retry_after.and_then(|value| parse_retry_after(value, now)),
        native_error: native_error.map(Box::new),
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
    fn normalizes_openai_style_errors_and_retry_delay() {
        let error = normalize_http_error(
            429,
            "Too Many Requests",
            r#"{"error":{"message":"slow down","type":"rate_limit_error","code":"limited"}}"#,
            Some("1.5"),
            UNIX_EPOCH,
        );
        assert_eq!(error.message, "slow down");
        assert_eq!(error.provider_type.as_deref(), Some("rate_limit_error"));
        assert_eq!(error.provider_code.as_deref(), Some("limited"));
        assert_eq!(error.retry_after_ms, Some(1_500));
        assert!(error.can_retry);
    }
}
