use std::sync::Arc;

use axum::http::{HeaderMap, header::AUTHORIZATION};
use subtle::ConstantTimeEq;
use zeroize::Zeroizing;

const BEARER_PREFIX: &[u8] = b"Bearer ";
const MINIMUM_TOKEN_BYTES: usize = 32;
const MAXIMUM_TOKEN_BYTES: usize = 512;

#[derive(Clone)]
pub struct AccessToken {
    value: Arc<Zeroizing<Vec<u8>>>,
}

impl AccessToken {
    pub fn new(value: impl AsRef<[u8]>) -> Result<Self, AccessTokenError> {
        let value = value.as_ref();
        if value.len() < MINIMUM_TOKEN_BYTES {
            return Err(AccessTokenError::TooShort);
        }
        if value.len() > MAXIMUM_TOKEN_BYTES {
            return Err(AccessTokenError::TooLong);
        }
        if value.iter().any(u8::is_ascii_whitespace) {
            return Err(AccessTokenError::ContainsWhitespace);
        }
        Ok(Self {
            value: Arc::new(Zeroizing::new(value.to_vec())),
        })
    }

    #[must_use]
    pub fn authorizes(&self, headers: &HeaderMap) -> bool {
        let Some(value) = headers.get(AUTHORIZATION) else {
            return false;
        };
        let Some(candidate) = value.as_bytes().strip_prefix(BEARER_PREFIX) else {
            return false;
        };
        candidate.len() == self.value.len() && bool::from(candidate.ct_eq(self.value.as_slice()))
    }
}

#[derive(Debug, thiserror::Error)]
pub enum AccessTokenError {
    #[error("access tokens must contain at least 32 bytes")]
    TooShort,
    #[error("access tokens must not exceed 512 bytes")]
    TooLong,
    #[error("access tokens must not contain whitespace")]
    ContainsWhitespace,
}

#[cfg(test)]
mod tests {
    use axum::http::{HeaderMap, HeaderValue, header::AUTHORIZATION};

    use super::AccessToken;

    #[test]
    fn accepts_only_the_exact_bearer_token() {
        let token = AccessToken::new("a-secure-gateway-token-that-is-long-enough").unwrap();
        let mut headers = HeaderMap::new();
        headers.insert(
            AUTHORIZATION,
            HeaderValue::from_static("Bearer a-secure-gateway-token-that-is-long-enough"),
        );
        assert!(token.authorizes(&headers));
        headers.insert(
            AUTHORIZATION,
            HeaderValue::from_static("Bearer a-secure-gateway-token-that-is-long-enougg"),
        );
        assert!(!token.authorizes(&headers));
    }
}
