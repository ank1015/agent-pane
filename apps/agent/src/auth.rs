use std::sync::Arc;

use axum::{
    extract::{Request, State},
    http::{HeaderMap, HeaderValue, header::AUTHORIZATION},
    middleware::Next,
    response::{IntoResponse as _, Response},
};
use subtle::ConstantTimeEq;
use zeroize::Zeroizing;

use crate::api_error::ApiError;

const BEARER_PREFIX: &[u8] = b"Bearer ";
const MINIMUM_TOKEN_BYTES: usize = 32;

#[derive(Clone)]
pub struct ControlToken {
    value: Arc<Zeroizing<Vec<u8>>>,
}

#[derive(Clone)]
pub struct WorkerToken {
    value: Arc<Zeroizing<Vec<u8>>>,
}

impl ControlToken {
    pub fn new(value: impl AsRef<[u8]>) -> Result<Self, ControlTokenError> {
        let value = value.as_ref();
        if value.len() < MINIMUM_TOKEN_BYTES {
            return Err(ControlTokenError::TooShort);
        }
        if value.iter().any(u8::is_ascii_whitespace) {
            return Err(ControlTokenError::ContainsWhitespace);
        }
        Ok(Self {
            value: Arc::new(Zeroizing::new(value.to_vec())),
        })
    }

    #[must_use]
    pub fn authorizes(&self, headers: &HeaderMap) -> bool {
        authorizes(&self.value, headers)
    }
}

impl WorkerToken {
    pub fn new(value: impl AsRef<[u8]>) -> Result<Self, WorkerTokenError> {
        let value = value.as_ref();
        if value.len() < MINIMUM_TOKEN_BYTES {
            return Err(WorkerTokenError::TooShort);
        }
        if value.iter().any(u8::is_ascii_whitespace) {
            return Err(WorkerTokenError::ContainsWhitespace);
        }
        Ok(Self {
            value: Arc::new(Zeroizing::new(value.to_vec())),
        })
    }

    #[must_use]
    pub fn authorizes(&self, headers: &HeaderMap) -> bool {
        authorizes(&self.value, headers)
    }
}

pub async fn require_control(
    State(token): State<ControlToken>,
    headers: HeaderMap,
    request: Request,
    next: Next,
) -> Response {
    if token.authorizes(&headers) {
        return next.run(request).await;
    }
    unauthorized(ApiError::unauthorized_control())
}

pub async fn require_worker(
    State(token): State<WorkerToken>,
    headers: HeaderMap,
    request: Request,
    next: Next,
) -> Response {
    if token.authorizes(&headers) {
        return next.run(request).await;
    }
    unauthorized(ApiError::unauthorized_worker())
}

fn authorizes(secret: &[u8], headers: &HeaderMap) -> bool {
    let Some(value) = headers.get(AUTHORIZATION) else {
        return false;
    };
    let Some(candidate) = value.as_bytes().strip_prefix(BEARER_PREFIX) else {
        return false;
    };
    candidate.len() == secret.len() && bool::from(candidate.ct_eq(secret))
}

fn unauthorized(error: ApiError) -> Response {
    let mut response = error.into_response();
    response.headers_mut().insert(
        axum::http::header::WWW_AUTHENTICATE,
        HeaderValue::from_static("Bearer"),
    );
    response
}

#[derive(Debug, thiserror::Error)]
pub enum ControlTokenError {
    #[error("AGENT_CONTROL_TOKEN must contain at least 32 bytes")]
    TooShort,
    #[error("AGENT_CONTROL_TOKEN must not contain whitespace")]
    ContainsWhitespace,
}

#[derive(Debug, thiserror::Error)]
pub enum WorkerTokenError {
    #[error("AGENT_WORKER_TOKEN must contain at least 32 bytes")]
    TooShort,
    #[error("AGENT_WORKER_TOKEN must not contain whitespace")]
    ContainsWhitespace,
}

#[cfg(test)]
mod tests {
    use axum::http::{HeaderMap, HeaderValue, header::AUTHORIZATION};

    use super::{ControlToken, WorkerToken};

    const CONTROL: &str = "agent-control-token-that-is-long-enough";
    const WORKER: &str = "agent-worker-token-that-is-long-enough";

    #[test]
    fn accepts_only_the_exact_control_bearer_token() {
        let token = ControlToken::new(CONTROL).expect("control token");
        let mut headers = HeaderMap::new();
        headers.insert(
            AUTHORIZATION,
            HeaderValue::from_str(&format!("Bearer {CONTROL}")).expect("authorization header"),
        );
        assert!(token.authorizes(&headers));

        headers.insert(
            AUTHORIZATION,
            HeaderValue::from_static("Bearer wrong-token-that-is-still-long-enough"),
        );
        assert!(!token.authorizes(&headers));
    }

    #[test]
    fn keeps_control_and_worker_tokens_independent() {
        let worker = WorkerToken::new(WORKER).expect("worker token");
        let mut headers = HeaderMap::new();
        headers.insert(
            AUTHORIZATION,
            HeaderValue::from_str(&format!("Bearer {CONTROL}")).expect("authorization header"),
        );
        assert!(!worker.authorizes(&headers));
    }

    #[test]
    fn rejects_short_or_whitespace_tokens() {
        assert!(ControlToken::new("short").is_err());
        assert!(WorkerToken::new("agent-worker-token-that has whitespace").is_err());
    }
}
