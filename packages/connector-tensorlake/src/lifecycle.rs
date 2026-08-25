use std::time::{Duration, Instant};

use reqwest::{Client, Response, Url};
use serde::Deserialize;
use serde_json::json;

use crate::TensorlakeTransportError;

const API_URL: &str = "https://api.tensorlake.ai/";

/// Creates a Tensorlake sandbox from a saved snapshot and returns its sandbox ID.
pub async fn create_from_snapshot(
    api_key: &str,
    snapshot_id: &str,
) -> Result<String, TensorlakeTransportError> {
    create_from_snapshot_at(&Client::new(), api_url(), api_key, snapshot_id).await
}

#[derive(Clone, Debug)]
pub struct ReadyTensorlakeSandbox {
    pub sandbox_url: Url,
}

/// Waits until Tensorlake reports a running sandbox and returns its management URL.
pub async fn wait_until_ready(
    api_key: &str,
    sandbox_id: &str,
    timeout: Duration,
    poll_interval: Duration,
) -> Result<ReadyTensorlakeSandbox, TensorlakeTransportError> {
    wait_until_ready_at(
        &Client::new(),
        api_url(),
        api_key,
        sandbox_id,
        timeout,
        poll_interval,
    )
    .await
}

/// Terminates a Tensorlake sandbox. A missing sandbox is treated as terminated.
pub async fn terminate(api_key: &str, sandbox_id: &str) -> Result<(), TensorlakeTransportError> {
    terminate_at(&Client::new(), api_url(), api_key, sandbox_id).await
}

async fn create_from_snapshot_at(
    client: &Client,
    base_url: Url,
    api_key: &str,
    snapshot_id: &str,
) -> Result<String, TensorlakeTransportError> {
    validate(api_key, "API key")?;
    validate(snapshot_id, "snapshot ID")?;
    let response = client
        .post(endpoint(base_url, "sandboxes")?)
        .bearer_auth(api_key)
        .json(&json!({ "snapshot_id": snapshot_id }))
        .send()
        .await
        .map_err(request_error)?;
    checked_json::<CreateSandboxResponse>(response)
        .await
        .map(|response| response.sandbox_id)
}

#[derive(Deserialize)]
struct CreateSandboxResponse {
    sandbox_id: String,
}

#[derive(Deserialize)]
struct SandboxResponse {
    status: String,
    #[serde(default)]
    sandbox_url: Option<String>,
    #[serde(default)]
    outcome: Option<String>,
}

#[allow(clippy::too_many_arguments)]
async fn wait_until_ready_at(
    client: &Client,
    base_url: Url,
    api_key: &str,
    sandbox_id: &str,
    timeout: Duration,
    poll_interval: Duration,
) -> Result<ReadyTensorlakeSandbox, TensorlakeTransportError> {
    validate(api_key, "API key")?;
    validate(sandbox_id, "sandbox ID")?;
    let deadline = Instant::now() + timeout;
    loop {
        let response = client
            .get(endpoint(
                base_url.clone(),
                &format!("sandboxes/{sandbox_id}"),
            )?)
            .bearer_auth(api_key)
            .send()
            .await
            .map_err(request_error)?;
        let sandbox = checked_json::<SandboxResponse>(response).await?;
        match sandbox.status.as_str() {
            "running" => {
                let url = sandbox.sandbox_url.ok_or_else(|| {
                    TensorlakeTransportError::new(
                        "Tensorlake running sandbox response omitted sandbox_url",
                    )
                })?;
                return Url::parse(&url)
                    .map(|sandbox_url| ReadyTensorlakeSandbox { sandbox_url })
                    .map_err(|source| {
                        TensorlakeTransportError::new(format!(
                            "Tensorlake returned an invalid sandbox URL: {source}"
                        ))
                    });
            }
            "terminated" => {
                return Err(TensorlakeTransportError::new(format!(
                    "Tensorlake sandbox terminated during provisioning{}",
                    sandbox
                        .outcome
                        .as_deref()
                        .map(|outcome| format!(": {outcome}"))
                        .unwrap_or_default()
                )));
            }
            _ if Instant::now() >= deadline => {
                return Err(TensorlakeTransportError {
                    message: "timed out waiting for Tensorlake sandbox to run".to_owned(),
                    retryable: true,
                    disconnected: false,
                });
            }
            _ => tokio::time::sleep(poll_interval).await,
        }
    }
}

async fn terminate_at(
    client: &Client,
    base_url: Url,
    api_key: &str,
    sandbox_id: &str,
) -> Result<(), TensorlakeTransportError> {
    validate(api_key, "API key")?;
    validate(sandbox_id, "sandbox ID")?;
    let response = client
        .delete(endpoint(base_url, &format!("sandboxes/{sandbox_id}"))?)
        .bearer_auth(api_key)
        .send()
        .await
        .map_err(request_error)?;
    if response.status().is_success() || response.status().as_u16() == 404 {
        Ok(())
    } else {
        Err(response_error(response).await)
    }
}

fn api_url() -> Url {
    Url::parse(API_URL).expect("Tensorlake API URL is valid")
}

fn endpoint(mut base_url: Url, segment: &str) -> Result<Url, TensorlakeTransportError> {
    base_url
        .path_segments_mut()
        .map_err(|()| TensorlakeTransportError::new("Tensorlake API URL cannot be a base URL"))?
        .pop_if_empty()
        .extend(segment.split('/'));
    Ok(base_url)
}

fn validate(value: &str, field: &str) -> Result<(), TensorlakeTransportError> {
    if value.is_empty() || value != value.trim() {
        return Err(TensorlakeTransportError::new(format!(
            "Tensorlake {field} must not be blank or have surrounding whitespace"
        )));
    }
    Ok(())
}

async fn checked_json<T: serde::de::DeserializeOwned>(
    response: Response,
) -> Result<T, TensorlakeTransportError> {
    if !response.status().is_success() {
        return Err(response_error(response).await);
    }
    response.json().await.map_err(request_error)
}

async fn response_error(response: Response) -> TensorlakeTransportError {
    let status = response.status();
    let body = response.text().await.unwrap_or_default();
    TensorlakeTransportError {
        message: format!("Tensorlake API returned {status}: {body}"),
        retryable: status.is_server_error() || status.as_u16() == 429,
        disconnected: false,
    }
}

fn request_error(source: reqwest::Error) -> TensorlakeTransportError {
    TensorlakeTransportError {
        message: source.to_string(),
        retryable: source.is_timeout() || source.is_connect(),
        disconnected: source.is_connect() || source.is_timeout() || source.is_body(),
    }
}

#[cfg(test)]
mod tests {
    use axum::{Json, Router, http::HeaderMap, http::StatusCode, routing::post};
    use serde_json::{Value, json};
    use tokio::net::TcpListener;

    use super::*;

    #[tokio::test]
    async fn creates_a_sandbox_from_the_snapshot_reference() {
        async fn create(headers: HeaderMap, Json(body): Json<Value>) -> Json<Value> {
            assert_eq!(headers["authorization"], "Bearer tensorlake-secret");
            assert_eq!(body, json!({ "snapshot_id": "snapshot-1" }));
            Json(json!({ "sandbox_id": "sandbox-1", "status": "pending" }))
        }

        let base = serve(Router::new().route("/sandboxes", post(create))).await;
        let sandbox_id =
            create_from_snapshot_at(&Client::new(), base, "tensorlake-secret", "snapshot-1")
                .await
                .unwrap();
        assert_eq!(sandbox_id, "sandbox-1");
    }

    #[tokio::test]
    async fn resolves_the_running_sandbox_url_and_terminates() {
        async fn get(headers: HeaderMap) -> Json<Value> {
            assert_eq!(headers["authorization"], "Bearer tensorlake-secret");
            Json(json!({
                "status": "running",
                "sandbox_url": "https://sandbox.tensorlake.example/"
            }))
        }
        async fn delete(headers: HeaderMap) -> StatusCode {
            assert_eq!(headers["authorization"], "Bearer tensorlake-secret");
            StatusCode::NO_CONTENT
        }
        let base = serve(Router::new().route(
            "/sandboxes/sandbox-1",
            axum::routing::get(get).delete(delete),
        ))
        .await;
        let ready = wait_until_ready_at(
            &Client::new(),
            base.clone(),
            "tensorlake-secret",
            "sandbox-1",
            Duration::from_secs(1),
            Duration::from_millis(1),
        )
        .await
        .unwrap();
        assert_eq!(
            ready.sandbox_url.as_str(),
            "https://sandbox.tensorlake.example/"
        );
        terminate_at(&Client::new(), base, "tensorlake-secret", "sandbox-1")
            .await
            .unwrap();
    }

    async fn serve(app: Router) -> Url {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        Url::parse(&format!("http://{address}/")).unwrap()
    }
}
