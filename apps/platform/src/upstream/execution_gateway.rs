use std::time::Duration;

use axum::http::StatusCode;
use reqwest::header;
use serde::de::DeserializeOwned;
use url::Url;
use uuid::Uuid;

use crate::{
    error::execution_gateway_rejected_body,
    machines::model::{
        CreateSandboxAccountRequest, MachineInventory, RotateSandboxCredentialsRequest,
        SandboxAccount, UpdateNameRequest,
    },
};
use execution_protocol::MachineSummary;

#[derive(Clone)]
pub struct ExecutionGatewayClient {
    http: reqwest::Client,
    base_url: Url,
}

impl ExecutionGatewayClient {
    pub fn new(
        base_url: Url,
        control_token: &str,
        timeout: Duration,
    ) -> Result<Self, ExecutionGatewayClientError> {
        let mut authorization =
            reqwest::header::HeaderValue::from_str(&format!("Bearer {control_token}"))
                .map_err(ExecutionGatewayClientError::InvalidControlToken)?;
        authorization.set_sensitive(true);
        let mut headers = reqwest::header::HeaderMap::new();
        headers.insert(header::AUTHORIZATION, authorization);

        let http = reqwest::Client::builder()
            .default_headers(headers)
            .timeout(timeout)
            .build()
            .map_err(ExecutionGatewayClientError::Build)?;

        Ok(Self {
            http,
            base_url: normalized_base_url(base_url),
        })
    }

    pub(crate) async fn ready(&self) -> Result<(), ExecutionGatewayError> {
        self.send_empty(self.http.get(self.url("health")?)).await
    }

    pub(crate) async fn list_sandbox_accounts(
        &self,
    ) -> Result<Vec<SandboxAccount>, ExecutionGatewayError> {
        self.send_json(self.http.get(self.url("v1/control/sandbox-accounts")?))
            .await
    }

    pub(crate) async fn machine_inventory(
        &self,
    ) -> Result<MachineInventory, ExecutionGatewayError> {
        self.send_json(self.http.get(self.url("v1/control/machines")?))
            .await
    }

    pub(crate) async fn get_sandbox_account(
        &self,
        account_id: Uuid,
    ) -> Result<SandboxAccount, ExecutionGatewayError> {
        self.send_json(
            self.http
                .get(self.url(&format!("v1/control/sandbox-accounts/{account_id}"))?),
        )
        .await
    }

    pub(crate) async fn create_sandbox_account(
        &self,
        request: &CreateSandboxAccountRequest,
    ) -> Result<SandboxAccount, ExecutionGatewayError> {
        self.send_json(
            self.http
                .post(self.url("v1/control/sandbox-accounts")?)
                .json(request),
        )
        .await
    }

    pub(crate) async fn rotate_sandbox_credentials(
        &self,
        account_id: Uuid,
        request: &RotateSandboxCredentialsRequest,
    ) -> Result<SandboxAccount, ExecutionGatewayError> {
        self.send_json(
            self.http
                .put(self.url(&format!(
                    "v1/control/sandbox-accounts/{account_id}/credentials"
                ))?)
                .json(request),
        )
        .await
    }

    pub(crate) async fn update_sandbox_account_name(
        &self,
        account_id: Uuid,
        request: &UpdateNameRequest,
    ) -> Result<SandboxAccount, ExecutionGatewayError> {
        self.send_json(
            self.http
                .patch(self.url(&format!("v1/control/sandbox-accounts/{account_id}"))?)
                .json(request),
        )
        .await
    }

    pub(crate) async fn delete_sandbox_account(
        &self,
        account_id: Uuid,
    ) -> Result<(), ExecutionGatewayError> {
        self.send_empty(
            self.http
                .delete(self.url(&format!("v1/control/sandbox-accounts/{account_id}"))?),
        )
        .await
    }

    pub(crate) async fn update_machine_name(
        &self,
        machine_id: &str,
        request: &UpdateNameRequest,
    ) -> Result<MachineSummary, ExecutionGatewayError> {
        self.send_json(
            self.http
                .patch(self.url(&format!("v1/control/machines/{machine_id}"))?)
                .json(request),
        )
        .await
    }

    pub(crate) async fn delete_machine(
        &self,
        machine_id: &str,
    ) -> Result<(), ExecutionGatewayError> {
        self.send_empty(
            self.http
                .delete(self.url(&format!("v1/control/machines/{machine_id}"))?),
        )
        .await
    }

    fn url(&self, path: &str) -> Result<Url, ExecutionGatewayError> {
        self.base_url
            .join(path)
            .map_err(ExecutionGatewayError::InvalidUrl)
    }

    async fn send_json<T>(
        &self,
        request: reqwest::RequestBuilder,
    ) -> Result<T, ExecutionGatewayError>
    where
        T: DeserializeOwned,
    {
        let response = request
            .send()
            .await
            .map_err(ExecutionGatewayError::Request)?;
        let status = response.status();
        let body = response
            .bytes()
            .await
            .map_err(ExecutionGatewayError::Request)?;
        if !status.is_success() {
            return Err(ExecutionGatewayError::Rejected {
                status,
                body: execution_gateway_rejected_body(&body),
            });
        }
        serde_json::from_slice(&body).map_err(ExecutionGatewayError::InvalidResponse)
    }

    async fn send_empty(
        &self,
        request: reqwest::RequestBuilder,
    ) -> Result<(), ExecutionGatewayError> {
        let response = request
            .send()
            .await
            .map_err(ExecutionGatewayError::Request)?;
        let status = response.status();
        if status.is_success() {
            return Ok(());
        }
        let body = response
            .bytes()
            .await
            .map_err(ExecutionGatewayError::Request)?;
        Err(ExecutionGatewayError::Rejected {
            status,
            body: execution_gateway_rejected_body(&body),
        })
    }
}

fn normalized_base_url(mut base_url: Url) -> Url {
    if !base_url.path().ends_with('/') {
        let path = format!("{}/", base_url.path());
        base_url.set_path(&path);
    }
    base_url
}

#[derive(Debug, thiserror::Error)]
pub enum ExecutionGatewayClientError {
    #[error("PLATFORM_EXECUTION_GATEWAY_CONTROL_TOKEN is not a valid HTTP header value")]
    InvalidControlToken(#[source] reqwest::header::InvalidHeaderValue),
    #[error("could not build the execution gateway HTTP client")]
    Build(#[source] reqwest::Error),
}

#[derive(Debug, thiserror::Error)]
pub enum ExecutionGatewayError {
    #[error("could not construct an execution gateway URL")]
    InvalidUrl(#[source] url::ParseError),
    #[error("execution gateway request failed")]
    Request(#[source] reqwest::Error),
    #[error("execution gateway returned an invalid response")]
    InvalidResponse(#[source] serde_json::Error),
    #[error("execution gateway rejected the request with status {status}")]
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
        extract::State,
        http::{HeaderMap, StatusCode},
        routing::post,
    };
    use serde_json::{Value, json};
    use tokio::{net::TcpListener, sync::mpsc};

    use super::ExecutionGatewayClient;
    use crate::machines::model::{CreateSandboxAccountRequest, MachineInventory, SandboxProvider};

    #[tokio::test]
    async fn create_sandbox_account_authenticates_and_forwards_the_secret_once() {
        let (request_tx, mut request_rx) = mpsc::unbounded_channel();
        let mock_gateway = Router::new()
            .route("/v1/control/sandbox-accounts", post(capture_create_request))
            .with_state(request_tx);
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        tokio::spawn(async move {
            axum::serve(listener, mock_gateway).await.unwrap();
        });

        let client = ExecutionGatewayClient::new(
            format!("http://{address}").parse().unwrap(),
            "platform-control-token",
            Duration::from_secs(1),
        )
        .unwrap();
        let response = client
            .create_sandbox_account(&CreateSandboxAccountRequest {
                provider: SandboxProvider::E2b,
                name: "main".to_owned(),
                api_key: "secret-key".to_owned(),
            })
            .await
            .unwrap();
        let (authorization, body) = request_rx.recv().await.unwrap();

        assert_eq!(authorization, "Bearer platform-control-token");
        assert_eq!(
            body,
            json!({"provider": "e2b", "name": "main", "api_key": "secret-key"})
        );
        assert_eq!(response.name, "main");
    }

    #[tokio::test]
    async fn machine_inventory_uses_the_control_endpoint() {
        let (request_tx, mut request_rx) = mpsc::unbounded_channel();
        let mock_gateway = Router::new()
            .route(
                "/v1/control/machines",
                axum::routing::get(capture_inventory_request),
            )
            .with_state(request_tx);
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        tokio::spawn(async move {
            axum::serve(listener, mock_gateway).await.unwrap();
        });

        let client = ExecutionGatewayClient::new(
            format!("http://{address}").parse().unwrap(),
            "platform-control-token",
            Duration::from_secs(1),
        )
        .unwrap();
        let inventory: MachineInventory = client.machine_inventory().await.unwrap();

        assert_eq!(
            request_rx.recv().await.unwrap(),
            "Bearer platform-control-token"
        );
        assert!(inventory.connector_accounts.is_empty());
        assert!(inventory.machine_daemons.is_empty());
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
                "id": "01992aa0-0000-7000-8000-000000000001",
                "provider": "e2b",
                "name": "main",
                "config": {},
                "enabled": true,
                "is_default": true,
                "validation_status": "unchecked",
                "credential_version": 1,
                "credentials_updated_at": 1787443200000_u64,
                "created_at": 1787443200000_u64,
                "updated_at": 1787443200000_u64
            })),
        )
    }

    async fn capture_inventory_request(
        State(request_tx): State<mpsc::UnboundedSender<String>>,
        headers: HeaderMap,
    ) -> Json<Value> {
        let authorization = headers
            .get("authorization")
            .and_then(|value| value.to_str().ok())
            .unwrap_or_default()
            .to_owned();
        request_tx.send(authorization).unwrap();
        Json(json!({"connector_accounts": [], "machine_daemons": []}))
    }
}
