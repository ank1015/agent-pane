use std::sync::Arc;

use axum::http::{HeaderMap, header::AUTHORIZATION};
use subtle::ConstantTimeEq;
use zeroize::Zeroizing;

const BEARER_PREFIX: &[u8] = b"Bearer ";
const MINIMUM_ADMIN_TOKEN_BYTES: usize = 32;

#[derive(Clone)]
pub struct AdminToken {
    value: Arc<Zeroizing<Vec<u8>>>,
}

impl AdminToken {
    pub fn new(value: impl AsRef<[u8]>) -> Result<Self, AdminTokenError> {
        let value = value.as_ref();
        if value.len() < MINIMUM_ADMIN_TOKEN_BYTES {
            return Err(AdminTokenError::TooShort);
        }
        if value.iter().any(u8::is_ascii_whitespace) {
            return Err(AdminTokenError::ContainsWhitespace);
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
        let bytes = value.as_bytes();
        let Some(candidate) = bytes.strip_prefix(BEARER_PREFIX) else {
            return false;
        };
        candidate.len() == self.value.len() && bool::from(candidate.ct_eq(self.value.as_slice()))
    }
}

#[derive(Debug, thiserror::Error)]
pub enum AdminTokenError {
    #[error("GATEWAY_ADMIN_TOKEN must contain at least 32 bytes")]
    TooShort,
    #[error("GATEWAY_ADMIN_TOKEN must not contain whitespace")]
    ContainsWhitespace,
}

#[cfg(test)]
mod tests {
    use axum::http::{HeaderMap, HeaderValue, header::AUTHORIZATION};

    use super::AdminToken;

    #[test]
    fn accepts_only_the_exact_bearer_token() {
        let token = AdminToken::new("a-secure-admin-token-that-is-long-enough").unwrap();
        let mut headers = HeaderMap::new();
        headers.insert(
            AUTHORIZATION,
            HeaderValue::from_static("Bearer a-secure-admin-token-that-is-long-enough"),
        );
        assert!(token.authorizes(&headers));

        headers.insert(
            AUTHORIZATION,
            HeaderValue::from_static("Bearer a-secure-admin-token-that-is-long-enougg"),
        );
        assert!(!token.authorizes(&headers));
    }
}
