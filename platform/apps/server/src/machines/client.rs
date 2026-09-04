use std::time::Duration;

use execution_api::{E2bAccount, ExecutionHost};
use reqwest::{
    Client,
    header::{AUTHORIZATION, HeaderMap, HeaderValue},
};
use thiserror::Error;
use url::Url;

use super::CreateE2bAccountInput;
use crate::error::ApiError;

#[derive(Clone)]
pub struct ExecutionGatewayClient {
    http: Client,
    hosts_url: Url,
    accounts_url: Url,
}

impl ExecutionGatewayClient {
    pub fn new(base_url: Url, token: &str, timeout: Duration) -> Result<Self, ClientConfigError> {
        let base_url = normalized_base_url(base_url);
        let accounts_url = base_url
            .join("v1/e2b-accounts")
            .map_err(ClientConfigError::InvalidHostsUrl)?;
        let hosts_url = base_url
            .join("v1/hosts")
            .map_err(ClientConfigError::InvalidHostsUrl)?;
        let mut authorization = HeaderValue::from_str(&format!("Bearer {token}"))
            .map_err(ClientConfigError::InvalidToken)?;
        authorization.set_sensitive(true);
        let mut headers = HeaderMap::new();
        headers.insert(AUTHORIZATION, authorization);
        let http = Client::builder()
            // Never forward account credentials to a redirected destination.
            .redirect(reqwest::redirect::Policy::none())
            .default_headers(headers)
            .timeout(timeout)
            .build()
            .map_err(ClientConfigError::BuildClient)?;

        Ok(Self {
            http,
            hosts_url,
            accounts_url,
        })
    }

    pub async fn list_e2b_accounts(&self) -> Result<Vec<E2bAccount>, ApiError> {
        let response = self
            .http
            .get(self.accounts_url.clone())
            .send()
            .await
            .map_err(ApiError::GatewayUnavailable)?;
        if !response.status().is_success() {
            return Err(ApiError::GatewayStatus(response.status()));
        }
        response
            .json()
            .await
            .map_err(ApiError::InvalidGatewayResponse)
    }

    pub async fn create_e2b_account(
        &self,
        request: &CreateE2bAccountInput,
    ) -> Result<E2bAccount, ApiError> {
        // POST is deliberately not retried: the gateway does not make account
        // creation idempotent. Its default is_default=false is preserved.
        let response = self
            .http
            .post(self.accounts_url.clone())
            .json(request)
            .send()
            .await
            .map_err(ApiError::GatewayUnavailable)?;
        if !response.status().is_success() {
            // Do not return or log upstream bodies, which may echo credentials.
            return Err(ApiError::AccountRejected(response.status()));
        }
        response
            .json()
            .await
            .map_err(ApiError::InvalidGatewayResponse)
    }

    pub async fn list_hosts(&self) -> Result<Vec<ExecutionHost>, ApiError> {
        let response = self
            .http
            .get(self.hosts_url.clone())
            .send()
            .await
            .map_err(ApiError::GatewayUnavailable)?;
        let status = response.status();
        if !status.is_success() {
            return Err(ApiError::GatewayStatus(status));
        }
        response
            .json()
            .await
            .map_err(ApiError::InvalidGatewayResponse)
    }
}

fn normalized_base_url(mut base_url: Url) -> Url {
    if !base_url.path().ends_with('/') {
        let path = format!("{}/", base_url.path());
        base_url.set_path(&path);
    }
    base_url
}

#[derive(Debug, Error)]
pub enum ClientConfigError {
    #[error("could not construct the execution gateway hosts URL")]
    InvalidHostsUrl(#[source] url::ParseError),
    #[error("PLATFORM_SERVER_EXECUTION_GATEWAY_TOKEN is not a valid HTTP header value")]
    InvalidToken(#[source] reqwest::header::InvalidHeaderValue),
    #[error("could not construct the execution gateway HTTP client")]
    BuildClient(#[source] reqwest::Error),
}

#[cfg(test)]
mod tests {
    use axum::{Json, Router, http::HeaderMap, routing::get};
    use serde::Serialize;
    use tokio::net::TcpListener;

    use super::*;

    #[tokio::test]
    async fn lists_hosts_with_the_configured_bearer_token() {
        async fn hosts(headers: HeaderMap) -> Json<Vec<TestHost>> {
            assert_eq!(
                headers
                    .get(AUTHORIZATION)
                    .and_then(|value| value.to_str().ok()),
                Some("Bearer test-token")
            );
            Json(vec![TestHost::registered()])
        }

        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let server = tokio::spawn(async move {
            axum::serve(listener, Router::new().route("/v1/hosts", get(hosts)))
                .await
                .unwrap();
        });
        let client = ExecutionGatewayClient::new(
            Url::parse(&format!("http://{address}")).unwrap(),
            "test-token",
            Duration::from_secs(2),
        )
        .unwrap();

        let hosts = client.list_hosts().await.unwrap();

        assert_eq!(hosts.len(), 1);
        assert_eq!(hosts[0].name.as_deref(), Some("Development Mac"));
        server.abort();
    }

    #[derive(Serialize)]
    struct TestHost {
        id: &'static str,
        kind: &'static str,
        name: &'static str,
        desired_state: &'static str,
        state: &'static str,
        status_code: Option<&'static str>,
        status_message: Option<&'static str>,
        status_retryable: bool,
        descriptor: Option<()>,
        metadata: serde_json::Value,
        e2b: Option<()>,
        registered: RegisteredBinding,
        last_seen_at: Option<&'static str>,
        revision: i64,
        created_at: &'static str,
        updated_at: &'static str,
        deleted_at: Option<&'static str>,
    }

    impl TestHost {
        fn registered() -> Self {
            Self {
                id: "018f47a8-80cc-7b2f-9d44-6657f5f82ad0",
                kind: "registered",
                name: "Development Mac",
                desired_state: "ready",
                state: "ready",
                status_code: None,
                status_message: None,
                status_retryable: false,
                descriptor: None,
                metadata: serde_json::json!({}),
                e2b: None,
                registered: RegisteredBinding::default(),
                last_seen_at: None,
                revision: 1,
                created_at: "2026-09-03T00:00:00Z",
                updated_at: "2026-09-03T00:00:00Z",
                deleted_at: None,
            }
        }
    }

    #[derive(Default, Serialize)]
    struct RegisteredBinding {
        installation_id: Option<&'static str>,
        daemon_version: Option<&'static str>,
        protocol_version: Option<u32>,
        registered_at: Option<&'static str>,
        last_connected_at: Option<&'static str>,
        last_disconnected_at: Option<&'static str>,
    }
}
