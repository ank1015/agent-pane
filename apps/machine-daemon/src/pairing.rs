use axum::http::{HeaderMap, header::AUTHORIZATION};
use subtle::ConstantTimeEq;

pub fn authorized(headers: &HeaderMap, expected_token: Option<&str>) -> bool {
    let Some(expected) = expected_token else {
        return true;
    };
    let Some(value) = headers
        .get(AUTHORIZATION)
        .and_then(|value| value.to_str().ok())
    else {
        return false;
    };
    value
        .strip_prefix("Bearer ")
        .is_some_and(|provided| bool::from(provided.as_bytes().ct_eq(expected.as_bytes())))
}

pub fn authorization_value(token: Option<&str>) -> Option<String> {
    token.map(|token| format!("Bearer {token}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bearer_token_is_required_when_configured() {
        let mut headers = HeaderMap::new();
        assert!(!authorized(&headers, Some("secret")));
        headers.insert(AUTHORIZATION, "Bearer secret".parse().expect("header"));
        assert!(authorized(&headers, Some("secret")));
        assert!(!authorized(&headers, Some("different")));
    }
}
