use axum::{
    extract::{Request, State},
    http::{HeaderValue, header::WWW_AUTHENTICATE},
    middleware::Next,
    response::Response,
};
use uuid::Uuid;

use crate::{auth::AccessToken, gateway::GatewayError, security::AuthenticationFailureLimiter};

use super::error_response;

#[derive(Clone)]
pub(super) struct AuthState {
    runtime: AccessToken,
    admin: AccessToken,
    failures: AuthenticationFailureLimiter,
}

impl AuthState {
    pub(super) const fn new(
        runtime: AccessToken,
        admin: AccessToken,
        failures: AuthenticationFailureLimiter,
    ) -> Self {
        Self {
            runtime,
            admin,
            failures,
        }
    }
}

pub(super) async fn require_token(
    State(tokens): State<AuthState>,
    request: Request,
    next: Next,
) -> Response {
    let path = request.uri().path();
    let token = if path == "/v1/admin" || path.starts_with("/v1/admin/") {
        &tokens.admin
    } else {
        &tokens.runtime
    };
    if token.authorizes(request.headers()) {
        return next.run(request).await;
    }
    let error = if tokens.failures.allow_failure() {
        GatewayError::unauthorized()
    } else {
        GatewayError::authentication_rate_limited()
    };
    let mut response = error_response(Uuid::now_v7(), error);
    response.headers_mut().insert(
        WWW_AUTHENTICATE,
        HeaderValue::from_static("Bearer realm=\"llm-gateway\""),
    );
    if response.status() == axum::http::StatusCode::TOO_MANY_REQUESTS {
        response.headers_mut().insert(
            axum::http::header::RETRY_AFTER,
            HeaderValue::from_static("60"),
        );
    }
    response
}
