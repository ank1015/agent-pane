use execution_api::ApiErrorBody;
use execution_core::{ExecutionError, ExecutionErrorCode};
use reqwest::StatusCode;

pub(crate) fn protocol_error(message: impl Into<String>) -> ExecutionError {
    ExecutionError::new(ExecutionErrorCode::Internal, message).with_detail("source", "protocol")
}

pub(crate) fn transport_error(error: reqwest::Error) -> ExecutionError {
    let code = if error.is_timeout() {
        ExecutionErrorCode::DeadlineExceeded
    } else {
        ExecutionErrorCode::Unavailable
    };
    // Do not include reqwest's URL or an arbitrary remote response body in errors.
    ExecutionError::new(code, "execution gateway transport failed")
        .retryable(error.is_connect())
        .with_detail("source", "transport")
}

pub(crate) fn gateway_error(
    status: StatusCode,
    bytes: &[u8],
    request_id: Option<&str>,
) -> ExecutionError {
    let body = serde_json::from_slice::<ApiErrorBody>(bytes).ok();
    let code = match body.as_ref().map(|body| body.error.code.as_str()) {
        Some("HOST_OPERATION_UNSUPPORTED") => ExecutionErrorCode::Unsupported,
        Some("HOST_NOT_READY" | "HOST_DISCONNECTED" | "E2B_ACCOUNT_UNAVAILABLE") => {
            ExecutionErrorCode::Unavailable
        }
        _ => match status {
            StatusCode::UNAUTHORIZED | StatusCode::FORBIDDEN => {
                ExecutionErrorCode::PermissionDenied
            }
            // A missing host is not a missing process session.
            StatusCode::NOT_FOUND => ExecutionErrorCode::NotFound,
            StatusCode::BAD_REQUEST | StatusCode::UNPROCESSABLE_ENTITY => {
                ExecutionErrorCode::InvalidRequest
            }
            StatusCode::CONFLICT => ExecutionErrorCode::OperationConflict,
            StatusCode::PAYLOAD_TOO_LARGE | StatusCode::TOO_MANY_REQUESTS => {
                ExecutionErrorCode::ResourceExhausted
            }
            StatusCode::REQUEST_TIMEOUT | StatusCode::GATEWAY_TIMEOUT => {
                ExecutionErrorCode::DeadlineExceeded
            }
            StatusCode::GONE | StatusCode::BAD_GATEWAY | StatusCode::SERVICE_UNAVAILABLE => {
                ExecutionErrorCode::Unavailable
            }
            _ => ExecutionErrorCode::Internal,
        },
    };
    let mut error = if let Some(body) = body {
        ExecutionError::new(code, body.error.message)
            .retryable(body.error.retryable)
            .with_detail("gateway_code", body.error.code)
            .with_detail("gateway_details", serde_json::json!(body.error.details))
    } else {
        ExecutionError::new(
            code,
            format!("execution gateway returned HTTP {}", status.as_u16()),
        )
    }
    .with_detail("source", "gateway")
    .with_detail("http_status", status.as_u16());
    if let Some(request_id) = request_id {
        error = error.with_detail("gateway_request_id", request_id);
    }
    error
}
