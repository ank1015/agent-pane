use axum::http::{HeaderMap, header::AUTHORIZATION};
use subtle::ConstantTimeEq;

pub fn bearer(headers: &HeaderMap, expected: &str) -> bool {
    headers
        .get(AUTHORIZATION)
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.strip_prefix("Bearer "))
        .is_some_and(|provided| bool::from(provided.as_bytes().ct_eq(expected.as_bytes())))
}

pub fn bearer_value(headers: &HeaderMap) -> Option<&str> {
    headers
        .get(AUTHORIZATION)
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.strip_prefix("Bearer "))
}
