use std::time::Duration;

use super::analytics::{GatewayUsageReport, ProviderRequestPage, RequestsQuery};
use reqwest::{
    Client,
    header::{AUTHORIZATION, HeaderMap, HeaderValue},
};
use thiserror::Error;
use url::Url;
use uuid::Uuid;

use super::{
    error::ProviderError,
    model::{GatewayAccountResponse, GatewayAccountsResponse, GatewayCreateAccount},
};

const MAX_RESPONSE_BYTES: usize = 4 * 1024 * 1024;

#[derive(Clone)]
pub struct LlmGatewayClient {
    http: Client,
    accounts_url: Url,
}

impl LlmGatewayClient {
    pub fn new(
        mut base_url: Url,
        admin_token: &str,
        timeout: Duration,
    ) -> Result<Self, ClientConfigError> {
        let loopback = matches!(
            base_url.host_str(),
            Some("localhost" | "127.0.0.1" | "[::1]")
        );
        if !(base_url.scheme() == "https" || (base_url.scheme() == "http" && loopback))
            || base_url.host_str().is_none()
            || !base_url.username().is_empty()
            || base_url.password().is_some()
            || base_url.query().is_some()
            || base_url.fragment().is_some()
        {
            return Err(ClientConfigError::InvalidBaseUrl);
        }
        if timeout.is_zero() {
            return Err(ClientConfigError::ZeroTimeout);
        }
        if admin_token.trim().is_empty() || admin_token.trim() != admin_token {
            return Err(ClientConfigError::EmptyAdminToken);
        }
        if !base_url.path().ends_with('/') {
            base_url.set_path(&format!("{}/", base_url.path()));
        }
        let accounts_url = base_url
            .join("v1/admin/accounts")
            .map_err(ClientConfigError::InvalidAccountsUrl)?;
        let mut authorization = HeaderValue::from_str(&format!("Bearer {admin_token}"))
            .map_err(ClientConfigError::InvalidAdminToken)?;
        authorization.set_sensitive(true);
        let mut headers = HeaderMap::new();
        headers.insert(AUTHORIZATION, authorization);
        headers.insert(
            reqwest::header::ACCEPT,
            HeaderValue::from_static("application/json"),
        );
        let http = Client::builder()
            .default_headers(headers)
            .redirect(reqwest::redirect::Policy::none())
            .timeout(timeout)
            .build()
            .map_err(ClientConfigError::BuildClient)?;
        Ok(Self { http, accounts_url })
    }

    pub(super) async fn list_accounts(&self) -> Result<GatewayAccountsResponse, ProviderError> {
        // No provider/status filter: return all configured accounts in one request.
        let response = self
            .http
            .get(self.accounts_url.clone())
            .send()
            .await
            .map_err(ProviderError::Request)?;
        Self::decode_response(response).await
    }

    pub(super) async fn get_account(
        &self,
        account_id: Uuid,
    ) -> Result<GatewayAccountResponse, ProviderError> {
        let mut account_url = self.accounts_url.clone();
        account_url
            .path_segments_mut()
            .expect("validated HTTP(S) URL supports path segments")
            .push(&account_id.to_string());
        let response = self
            .http
            .get(account_url)
            .send()
            .await
            .map_err(ProviderError::Request)?;
        if response.status() == reqwest::StatusCode::NOT_FOUND {
            return Err(ProviderError::NotFound);
        }
        Self::decode_response(response).await
    }

    pub(super) async fn create_account<T: serde::Serialize>(
        &self,
        input: &GatewayCreateAccount<'_, T>,
    ) -> Result<GatewayAccountResponse, ProviderError> {
        // Account creation is not idempotent: never retry this POST automatically.
        let response = self
            .http
            .post(self.accounts_url.clone())
            .json(input)
            .send()
            .await
            .map_err(ProviderError::Request)?;
        if matches!(response.status().as_u16(), 400 | 422) {
            return Err(ProviderError::InvalidRequest(
                "The gateway rejected the account. Check the provider credentials and try again.",
            ));
        }
        Self::decode_response(response).await
    }

    pub(super) async fn usage(&self, id: Uuid) -> Result<GatewayUsageReport, ProviderError> {
        self.account_analytics(id, "usage", None).await
    }

    pub(super) async fn requests(
        &self,
        id: Uuid,
        query: &RequestsQuery,
    ) -> Result<ProviderRequestPage, ProviderError> {
        self.account_analytics(id, "requests", Some(query)).await
    }

    async fn account_analytics<T: serde::de::DeserializeOwned>(
        &self,
        id: Uuid,
        resource: &str,
        query: Option<&RequestsQuery>,
    ) -> Result<T, ProviderError> {
        let mut url = self.accounts_url.clone();
        url.path_segments_mut()
            .expect("validated HTTP URL")
            .extend([&id.to_string(), resource]);
        let mut request = self.http.get(url);
        if let Some(query) = query {
            request = request.query(query);
        }
        let response = request.send().await.map_err(ProviderError::Request)?;
        match response.status() {
            reqwest::StatusCode::NOT_FOUND => return Err(ProviderError::NotFound),
            reqwest::StatusCode::BAD_REQUEST | reqwest::StatusCode::UNPROCESSABLE_ENTITY => {
                return Err(ProviderError::InvalidRequest(
                    "The gateway rejected the analytics query. Reload the page and try again.",
                ));
            }
            _ => {}
        }
        Self::decode_response(response).await
    }

    async fn decode_response<T: serde::de::DeserializeOwned>(
        mut response: reqwest::Response,
    ) -> Result<T, ProviderError> {
        if !response.status().is_success() {
            // Upstream bodies may contain sensitive details. Never log or forward them.
            return Err(ProviderError::GatewayStatus(response.status()));
        }
        if response
            .content_length()
            .is_some_and(|size| size > MAX_RESPONSE_BYTES as u64)
        {
            return Err(ProviderError::ResponseTooLarge);
        }
        let mut body = Vec::new();
        while let Some(chunk) = response.chunk().await.map_err(ProviderError::Request)? {
            if body.len() + chunk.len() > MAX_RESPONSE_BYTES {
                return Err(ProviderError::ResponseTooLarge);
            }
            body.extend_from_slice(&chunk);
        }
        serde_json::from_slice(&body).map_err(|_| ProviderError::InvalidResponse)
    }
}

#[derive(Debug, Error)]
pub enum ClientConfigError {
    #[error(
        "PLATFORM_SERVER_LLM_GATEWAY_URL must use HTTPS (loopback HTTP is allowed) and contain no credentials, query, or fragment"
    )]
    InvalidBaseUrl,
    #[error("could not construct the LLM gateway accounts URL")]
    InvalidAccountsUrl(#[source] url::ParseError),
    #[error(
        "PLATFORM_SERVER_LLM_GATEWAY_ADMIN_TOKEN must be non-empty and have no surrounding whitespace"
    )]
    EmptyAdminToken,
    #[error("PLATFORM_SERVER_LLM_GATEWAY_ADMIN_TOKEN is not a valid HTTP header value")]
    InvalidAdminToken(#[source] reqwest::header::InvalidHeaderValue),
    #[error("PLATFORM_SERVER_LLM_GATEWAY_TIMEOUT_SECONDS must be positive")]
    ZeroTimeout,
    #[error("could not construct the LLM gateway HTTP client")]
    BuildClient(#[source] reqwest::Error),
}
