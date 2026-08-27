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
pub struct HarnessToken {
    value: Arc<Zeroizing<Vec<u8>>>,
}

impl ControlToken {
    pub fn new(value: impl AsRef<[u8]>) -> Result<Self, ControlTokenError> {
        validate(value.as_ref()).map_err(ControlTokenError)?;
        Ok(Self {
            value: Arc::new(Zeroizing::new(value.as_ref().to_vec())),
        })
    }
    #[must_use]
    pub fn authorizes(&self, headers: &HeaderMap) -> bool {
        authorizes(&self.value, headers)
    }
}
impl HarnessToken {
    pub fn new(value: impl AsRef<[u8]>) -> Result<Self, HarnessTokenError> {
        validate(value.as_ref()).map_err(HarnessTokenError)?;
        Ok(Self {
            value: Arc::new(Zeroizing::new(value.as_ref().to_vec())),
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
        next.run(request).await
    } else {
        unauthorized(ApiError::unauthorized_control())
    }
}
pub async fn require_harness(
    State(token): State<HarnessToken>,
    headers: HeaderMap,
    request: Request,
    next: Next,
) -> Response {
    if token.authorizes(&headers) {
        next.run(request).await
    } else {
        unauthorized(ApiError::unauthorized_harness())
    }
}
fn validate(value: &[u8]) -> Result<(), TokenError> {
    if value.len() < MINIMUM_TOKEN_BYTES {
        return Err(TokenError::TooShort);
    }
    if value.iter().any(u8::is_ascii_whitespace) {
        return Err(TokenError::ContainsWhitespace);
    }
    Ok(())
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
#[derive(Debug)]
enum TokenError {
    TooShort,
    ContainsWhitespace,
}
#[derive(Debug, thiserror::Error)]
#[error("invalid AGENT_CONTROL_TOKEN: {0:?}")]
pub struct ControlTokenError(TokenError);
#[derive(Debug, thiserror::Error)]
#[error("invalid AGENT_HARNESS_TOKEN: {0:?}")]
pub struct HarnessTokenError(TokenError);

#[cfg(test)]
mod tests {
    use super::{ControlToken, HarnessToken};
    use axum::http::{HeaderMap, HeaderValue, header::AUTHORIZATION};
    #[test]
    fn tokens_are_exact_and_independent() {
        let control = "agent-control-token-that-is-long-enough";
        let harness = HarnessToken::new("agent-harness-token-that-is-long-enough").unwrap();
        let control_token = ControlToken::new(control).unwrap();
        let mut headers = HeaderMap::new();
        headers.insert(
            AUTHORIZATION,
            HeaderValue::from_str(&format!("Bearer {control}")).unwrap(),
        );
        assert!(control_token.authorizes(&headers));
        assert!(!harness.authorizes(&headers));
    }
}
