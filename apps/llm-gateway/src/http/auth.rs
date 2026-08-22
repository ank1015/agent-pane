use axum::{
    extract::{Request, State},
    http::{HeaderValue, header::WWW_AUTHENTICATE},
    middleware::Next,
    response::Response,
};
use uuid::Uuid;

use crate::{auth::AdminToken, gateway::GatewayError};

use super::error_response;

pub(super) async fn require_admin(
    State(token): State<AdminToken>,
    request: Request,
    next: Next,
) -> Response {
    if token.authorizes(request.headers()) {
        return next.run(request).await;
    }

    let mut response = error_response(Uuid::now_v7(), GatewayError::unauthorized());
    response.headers_mut().insert(
        WWW_AUTHENTICATE,
        HeaderValue::from_static("Bearer realm=\"llm-gateway-admin\""),
    );
    response
}
