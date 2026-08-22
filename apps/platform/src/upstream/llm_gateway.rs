use std::time::Duration;

use axum::http::StatusCode;
use chrono::{DateTime, Utc};
use reqwest::{Method, header};
use serde::Serialize;
use serde::de::DeserializeOwned;
use url::Url;
use uuid::Uuid;
use zeroize::{Zeroize, ZeroizeOnDrop};

use crate::{
    error::rejected_body,
    providers::model::{
        CreateProviderRequest, GatewayProviderResponse, GatewayProvidersResponse, ProviderKind,
        RotateCredentialsRequest, UpdateProviderRequest,
    },
};

#[derive(Clone)]
pub struct LlmGatewayClient {
    http: reqwest::Client,
    base_url: Url,
}

impl LlmGatewayClient {
    pub fn new(
        base_url: Url,
        admin_token: &str,
        timeout: Duration,
    ) -> Result<Self, LlmGatewayClientError> {
        let mut authorization =
            reqwest::header::HeaderValue::from_str(&format!("Bearer {admin_token}"))
                .map_err(LlmGatewayClientError::InvalidAdminToken)?;
        authorization.set_sensitive(true);
        let mut headers = reqwest::header::HeaderMap::new();
        headers.insert(header::AUTHORIZATION, authorization);

        let http = reqwest::Client::builder()
            .default_headers(headers)
            .timeout(timeout)
            .build()
            .map_err(LlmGatewayClientError::Build)?;

        Ok(Self {
            http,
            base_url: normalized_base_url(base_url),
        })
    }

    pub(crate) async fn ready(&self) -> Result<(), LlmGatewayError> {
        self.send_empty(self.http.get(self.url("ready")?)).await
    }

    pub(crate) async fn list_providers(
        &self,
        provider: Option<ProviderKind>,
    ) -> Result<GatewayProvidersResponse, LlmGatewayError> {
        let mut request = self.http.get(self.url("v1/admin/accounts")?);
        if let Some(provider) = provider {
            request = request.query(&[("provider", provider.as_str())]);
        }
        self.send_json(request).await
    }

    pub(crate) async fn get_provider(
        &self,
        provider_id: Uuid,
    ) -> Result<GatewayProviderResponse, LlmGatewayError> {
        self.send_json(
            self.http
                .get(self.url(&format!("v1/admin/accounts/{provider_id}"))?),
        )
        .await
    }

    pub(crate) async fn create_provider(
        &self,
        request: &CreateProviderRequest,
    ) -> Result<GatewayProviderResponse, LlmGatewayError> {
        let gateway_request = GatewayCreateProviderRequest {
            provider: request.provider.into(),
            name: &request.name,
            credentials: GatewayApiKeyCredentials {
                api_key: &request.api_key,
            },
        };
        self.send_json(
            self.http
                .post(self.url("v1/admin/accounts")?)
                .json(&gateway_request),
        )
        .await
    }

    pub(crate) async fn create_chatgpt_provider(
        &self,
        name: &str,
        credentials: &ChatGptGatewayCredentials,
    ) -> Result<GatewayProviderResponse, LlmGatewayError> {
        let gateway_request = GatewayCreateChatGptProviderRequest {
            provider: ProviderKind::Chatgpt,
            name,
            credentials,
        };
        self.send_json(
            self.http
                .post(self.url("v1/admin/accounts")?)
                .json(&gateway_request),
        )
        .await
    }

    pub(crate) async fn update_provider(
        &self,
        provider_id: Uuid,
        request: &UpdateProviderRequest,
    ) -> Result<GatewayProviderResponse, LlmGatewayError> {
        self.send_json(
            self.http
                .patch(self.url(&format!("v1/admin/accounts/{provider_id}"))?)
                .json(request),
        )
        .await
    }

    pub(crate) async fn set_default_provider(
        &self,
        provider_id: Uuid,
    ) -> Result<GatewayProviderResponse, LlmGatewayError> {
        self.send_json(self.http.request(
            Method::PUT,
            self.url(&format!("v1/admin/accounts/{provider_id}/default"))?,
        ))
        .await
    }

    pub(crate) async fn rotate_credentials(
        &self,
        provider_id: Uuid,
        request: &RotateCredentialsRequest,
    ) -> Result<GatewayProviderResponse, LlmGatewayError> {
        self.send_json(
            self.http
                .request(
                    Method::PUT,
                    self.url(&format!("v1/admin/accounts/{provider_id}/credentials"))?,
                )
                .json(request),
        )
        .await
    }

    pub(crate) async fn delete_provider(&self, provider_id: Uuid) -> Result<(), LlmGatewayError> {
        self.send_empty(
            self.http
                .delete(self.url(&format!("v1/admin/accounts/{provider_id}"))?),
        )
        .await
    }

    fn url(&self, path: &str) -> Result<Url, LlmGatewayError> {
        self.base_url
            .join(path)
            .map_err(LlmGatewayError::InvalidUrl)
    }

    async fn send_json<T>(&self, request: reqwest::RequestBuilder) -> Result<T, LlmGatewayError>
    where
        T: DeserializeOwned,
    {
        let response = request.send().await.map_err(LlmGatewayError::Request)?;
        let status = response.status();
        let body = response.bytes().await.map_err(LlmGatewayError::Request)?;
        if !status.is_success() {
            return Err(LlmGatewayError::Rejected {
                status,
                body: rejected_body(&body),
            });
        }
        serde_json::from_slice(&body).map_err(LlmGatewayError::InvalidResponse)
    }

    async fn send_empty(&self, request: reqwest::RequestBuilder) -> Result<(), LlmGatewayError> {
        let response = request.send().await.map_err(LlmGatewayError::Request)?;
        let status = response.status();
        if status.is_success() {
            return Ok(());
        }
        let body = response.bytes().await.map_err(LlmGatewayError::Request)?;
        Err(LlmGatewayError::Rejected {
            status,
            body: rejected_body(&body),
        })
    }
}

#[derive(Serialize)]
struct GatewayCreateProviderRequest<'a> {
    provider: ProviderKind,
    name: &'a str,
    credentials: GatewayApiKeyCredentials<'a>,
}

#[derive(Serialize)]
struct GatewayApiKeyCredentials<'a> {
    api_key: &'a str,
}

#[derive(Serialize)]
struct GatewayCreateChatGptProviderRequest<'a> {
    provider: ProviderKind,
    name: &'a str,
    credentials: &'a ChatGptGatewayCredentials,
}

#[derive(Serialize, Zeroize, ZeroizeOnDrop)]
pub(crate) struct ChatGptGatewayCredentials {
    pub id_token: String,
    pub access_token: String,
    pub refresh_token: String,
    pub account_id: String,
    #[zeroize(skip)]
    pub access_token_expires_at: DateTime<Utc>,
}

fn normalized_base_url(mut base_url: Url) -> Url {
    if !base_url.path().ends_with('/') {
        let path = format!("{}/", base_url.path());
        base_url.set_path(&path);
    }
    base_url
}

#[derive(Debug, thiserror::Error)]
pub enum LlmGatewayClientError {
    #[error("PLATFORM_LLM_GATEWAY_ADMIN_TOKEN is not a valid HTTP header value")]
    InvalidAdminToken(#[source] reqwest::header::InvalidHeaderValue),
    #[error("could not build the LLM gateway HTTP client")]
    Build(#[source] reqwest::Error),
}

#[derive(Debug, thiserror::Error)]
pub enum LlmGatewayError {
    #[error("could not construct an LLM gateway URL")]
    InvalidUrl(#[source] url::ParseError),
    #[error("LLM gateway request failed")]
    Request(#[source] reqwest::Error),
    #[error("LLM gateway returned an invalid response")]
    InvalidResponse(#[source] serde_json::Error),
    #[error("LLM gateway rejected the request with status {status}")]
    Rejected {
        status: StatusCode,
        body: serde_json::Value,
    },
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use axum::{
        Json, Router,
        extract::{Path, State},
        http::{HeaderMap, StatusCode},
        routing::{delete, patch, post, put},
    };
    use serde_json::{Value, json};
    use tokio::{net::TcpListener, sync::mpsc};
    use uuid::Uuid;

    use super::{ChatGptGatewayCredentials, LlmGatewayClient};
    use crate::providers::model::{
        ApiKeyProviderKind, CreateProviderRequest, UpdateProviderRequest,
    };

    #[tokio::test]
    async fn create_provider_translates_and_authenticates_the_dashboard_request() {
        let (request_tx, mut request_rx) = mpsc::unbounded_channel();
        let mock_gateway = Router::new()
            .route("/v1/admin/accounts", post(capture_create_request))
            .with_state(request_tx);
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        tokio::spawn(async move {
            axum::serve(listener, mock_gateway).await.unwrap();
        });

        let client = LlmGatewayClient::new(
            format!("http://{address}").parse().unwrap(),
            "platform-admin-token",
            Duration::from_secs(1),
        )
        .unwrap();
        let response = client
            .create_provider(&CreateProviderRequest {
                provider: ApiKeyProviderKind::Openai,
                name: "personal".to_owned(),
                api_key: "secret-key".to_owned(),
            })
            .await
            .unwrap();
        let (authorization, body) = request_rx.recv().await.unwrap();

        assert_eq!(authorization, "Bearer platform-admin-token");
        assert_eq!(
            body,
            json!({
                "provider": "openai",
                "name": "personal",
                "credentials": {"api_key": "secret-key"}
            })
        );
        assert_eq!(response.account.name, "personal");
    }

    #[tokio::test]
    async fn create_chatgpt_provider_forwards_the_complete_token_bundle() {
        let (request_tx, mut request_rx) = mpsc::unbounded_channel();
        let mock_gateway = Router::new()
            .route("/v1/admin/accounts", post(capture_create_request))
            .with_state(request_tx);
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        tokio::spawn(async move {
            axum::serve(listener, mock_gateway).await.unwrap();
        });

        let client = LlmGatewayClient::new(
            format!("http://{address}").parse().unwrap(),
            "platform-admin-token",
            Duration::from_secs(1),
        )
        .unwrap();
        client
            .create_chatgpt_provider(
                "personal",
                &ChatGptGatewayCredentials {
                    id_token: "id-token".to_owned(),
                    access_token: "access-token".to_owned(),
                    refresh_token: "refresh-token".to_owned(),
                    account_id: "chatgpt-account".to_owned(),
                    access_token_expires_at: "2026-08-23T01:00:00Z".parse().unwrap(),
                },
            )
            .await
            .unwrap();
        let (authorization, body) = request_rx.recv().await.unwrap();

        assert_eq!(authorization, "Bearer platform-admin-token");
        assert_eq!(
            body,
            json!({
                "provider": "chatgpt",
                "name": "personal",
                "credentials": {
                    "id_token": "id-token",
                    "access_token": "access-token",
                    "refresh_token": "refresh-token",
                    "account_id": "chatgpt-account",
                    "access_token_expires_at": "2026-08-23T01:00:00Z"
                }
            })
        );
    }

    #[tokio::test]
    async fn delete_provider_calls_the_authenticated_admin_endpoint() {
        let (request_tx, mut request_rx) = mpsc::unbounded_channel();
        let mock_gateway = Router::new()
            .route(
                "/v1/admin/accounts/{account_id}",
                delete(capture_delete_request),
            )
            .with_state(request_tx);
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        tokio::spawn(async move {
            axum::serve(listener, mock_gateway).await.unwrap();
        });

        let client = LlmGatewayClient::new(
            format!("http://{address}").parse().unwrap(),
            "platform-admin-token",
            Duration::from_secs(1),
        )
        .unwrap();
        let account_id = Uuid::parse_str("01992aa0-0000-7000-8000-000000000001").unwrap();

        client.delete_provider(account_id).await.unwrap();
        let (authorization, captured_account_id) = request_rx.recv().await.unwrap();

        assert_eq!(authorization, "Bearer platform-admin-token");
        assert_eq!(captured_account_id, account_id);
    }

    #[tokio::test]
    async fn update_provider_forwards_the_enabled_state_to_the_admin_endpoint() {
        let (request_tx, mut request_rx) = mpsc::unbounded_channel();
        let mock_gateway = Router::new()
            .route(
                "/v1/admin/accounts/{account_id}",
                patch(capture_update_request),
            )
            .with_state(request_tx);
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        tokio::spawn(async move {
            axum::serve(listener, mock_gateway).await.unwrap();
        });

        let client = LlmGatewayClient::new(
            format!("http://{address}").parse().unwrap(),
            "platform-admin-token",
            Duration::from_secs(1),
        )
        .unwrap();
        let account_id = Uuid::parse_str("01992aa0-0000-7000-8000-000000000001").unwrap();
        let response = client
            .update_provider(
                account_id,
                &UpdateProviderRequest {
                    name: None,
                    config: None,
                    enabled: Some(false),
                },
            )
            .await
            .unwrap();
        let (authorization, captured_account_id, body) = request_rx.recv().await.unwrap();

        assert_eq!(authorization, "Bearer platform-admin-token");
        assert_eq!(captured_account_id, account_id);
        assert_eq!(body, json!({"enabled": false}));
        assert!(!response.account.enabled);
    }

    #[tokio::test]
    async fn update_provider_forwards_the_name_to_the_admin_endpoint() {
        let (request_tx, mut request_rx) = mpsc::unbounded_channel();
        let mock_gateway = Router::new()
            .route(
                "/v1/admin/accounts/{account_id}",
                patch(capture_update_request),
            )
            .with_state(request_tx);
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        tokio::spawn(async move {
            axum::serve(listener, mock_gateway).await.unwrap();
        });

        let client = LlmGatewayClient::new(
            format!("http://{address}").parse().unwrap(),
            "platform-admin-token",
            Duration::from_secs(1),
        )
        .unwrap();
        let account_id = Uuid::parse_str("01992aa0-0000-7000-8000-000000000001").unwrap();

        client
            .update_provider(
                account_id,
                &UpdateProviderRequest {
                    name: Some("work".to_owned()),
                    config: None,
                    enabled: None,
                },
            )
            .await
            .unwrap();
        let (authorization, captured_account_id, body) = request_rx.recv().await.unwrap();

        assert_eq!(authorization, "Bearer platform-admin-token");
        assert_eq!(captured_account_id, account_id);
        assert_eq!(body, json!({"name": "work"}));
    }

    #[tokio::test]
    async fn set_default_provider_calls_the_authenticated_admin_endpoint() {
        let (request_tx, mut request_rx) = mpsc::unbounded_channel();
        let mock_gateway = Router::new()
            .route(
                "/v1/admin/accounts/{account_id}/default",
                put(capture_set_default_request),
            )
            .with_state(request_tx);
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        tokio::spawn(async move {
            axum::serve(listener, mock_gateway).await.unwrap();
        });

        let client = LlmGatewayClient::new(
            format!("http://{address}").parse().unwrap(),
            "platform-admin-token",
            Duration::from_secs(1),
        )
        .unwrap();
        let account_id = Uuid::parse_str("01992aa0-0000-7000-8000-000000000001").unwrap();

        let response = client.set_default_provider(account_id).await.unwrap();
        let (authorization, captured_account_id) = request_rx.recv().await.unwrap();

        assert_eq!(authorization, "Bearer platform-admin-token");
        assert_eq!(captured_account_id, account_id);
        assert!(response.account.is_default);
    }

    async fn capture_create_request(
        State(request_tx): State<mpsc::UnboundedSender<(String, Value)>>,
        headers: HeaderMap,
        Json(body): Json<Value>,
    ) -> (StatusCode, Json<Value>) {
        let authorization = headers
            .get("authorization")
            .and_then(|value| value.to_str().ok())
            .unwrap_or_default()
            .to_owned();
        request_tx.send((authorization, body)).unwrap();

        (
            StatusCode::CREATED,
            Json(json!({
                "account": {
                    "id": "01992aa0-0000-7000-8000-000000000001",
                    "provider": "openai",
                    "name": "personal",
                    "config": {},
                    "enabled": true,
                    "is_default": true,
                    "created_at": "2026-08-22T00:00:00Z",
                    "updated_at": "2026-08-22T00:00:00Z",
                    "credential": {
                        "version": 1,
                        "encryption_key_version": 1,
                        "updated_at": "2026-08-22T00:00:00Z"
                    }
                }
            })),
        )
    }

    async fn capture_delete_request(
        State(request_tx): State<mpsc::UnboundedSender<(String, Uuid)>>,
        Path(account_id): Path<Uuid>,
        headers: HeaderMap,
    ) -> StatusCode {
        let authorization = headers
            .get("authorization")
            .and_then(|value| value.to_str().ok())
            .unwrap_or_default()
            .to_owned();
        request_tx.send((authorization, account_id)).unwrap();
        StatusCode::NO_CONTENT
    }

    async fn capture_update_request(
        State(request_tx): State<mpsc::UnboundedSender<(String, Uuid, Value)>>,
        Path(account_id): Path<Uuid>,
        headers: HeaderMap,
        Json(body): Json<Value>,
    ) -> (StatusCode, Json<Value>) {
        let authorization = headers
            .get("authorization")
            .and_then(|value| value.to_str().ok())
            .unwrap_or_default()
            .to_owned();
        request_tx.send((authorization, account_id, body)).unwrap();

        (
            StatusCode::OK,
            Json(json!({
                "account": {
                    "id": account_id,
                    "provider": "openai",
                    "name": "personal",
                    "config": {},
                    "enabled": false,
                    "is_default": false,
                    "created_at": "2026-08-22T00:00:00Z",
                    "updated_at": "2026-08-22T00:00:00Z",
                    "credential": {
                        "version": 1,
                        "encryption_key_version": 1,
                        "updated_at": "2026-08-22T00:00:00Z"
                    }
                }
            })),
        )
    }

    async fn capture_set_default_request(
        State(request_tx): State<mpsc::UnboundedSender<(String, Uuid)>>,
        Path(account_id): Path<Uuid>,
        headers: HeaderMap,
    ) -> (StatusCode, Json<Value>) {
        let authorization = headers
            .get("authorization")
            .and_then(|value| value.to_str().ok())
            .unwrap_or_default()
            .to_owned();
        request_tx.send((authorization, account_id)).unwrap();

        (
            StatusCode::OK,
            Json(json!({
                "account": {
                    "id": account_id,
                    "provider": "openai",
                    "name": "personal",
                    "config": {},
                    "enabled": true,
                    "is_default": true,
                    "created_at": "2026-08-22T00:00:00Z",
                    "updated_at": "2026-08-22T00:00:00Z",
                    "credential": {
                        "version": 1,
                        "encryption_key_version": 1,
                        "updated_at": "2026-08-22T00:00:00Z"
                    }
                }
            })),
        )
    }
}
