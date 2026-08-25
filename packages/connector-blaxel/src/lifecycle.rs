use std::time::{Duration, Instant};

use reqwest::{Client, Response, Url};
use serde::Deserialize;
use serde_json::json;

use crate::BlaxelTransportError;

const API_URL: &str = "https://api.blaxel.ai/v0/";

/// Forks a Blaxel sandbox from a saved snapshot and returns the target sandbox ID.
pub async fn create_from_snapshot(
    api_key: &str,
    workspace: &str,
    source_sandbox_id: &str,
    snapshot_id: &str,
    target_sandbox_id: &str,
) -> Result<String, BlaxelTransportError> {
    create_from_snapshot_at(
        &Client::new(),
        api_url(),
        api_key,
        workspace,
        source_sandbox_id,
        snapshot_id,
        target_sandbox_id,
    )
    .await
}

#[derive(Clone, Debug)]
pub struct ReadyBlaxelSandbox {
    pub sandbox_url: Url,
}

/// Waits for the forked sandbox deployment and resolves its direct API URL.
pub async fn wait_until_ready(
    api_key: &str,
    workspace: &str,
    sandbox_id: &str,
    timeout: Duration,
    poll_interval: Duration,
) -> Result<ReadyBlaxelSandbox, BlaxelTransportError> {
    wait_until_ready_at(
        &Client::new(),
        api_url(),
        api_key,
        workspace,
        sandbox_id,
        timeout,
        poll_interval,
    )
    .await
}

/// Deletes a Blaxel sandbox. A missing sandbox is treated as deleted.
pub async fn terminate(
    api_key: &str,
    workspace: &str,
    sandbox_id: &str,
) -> Result<(), BlaxelTransportError> {
    terminate_at(&Client::new(), api_url(), api_key, workspace, sandbox_id).await
}

#[allow(clippy::too_many_arguments)]
async fn create_from_snapshot_at(
    client: &Client,
    base_url: Url,
    api_key: &str,
    workspace: &str,
    source_sandbox_id: &str,
    snapshot_id: &str,
    target_sandbox_id: &str,
) -> Result<String, BlaxelTransportError> {
    validate(api_key, "API key")?;
    validate(workspace, "workspace")?;
    validate(source_sandbox_id, "source sandbox ID")?;
    validate(snapshot_id, "snapshot ID")?;
    validate(target_sandbox_id, "target sandbox ID")?;
    let response = client
        .post(fork_endpoint(base_url, source_sandbox_id)?)
        .bearer_auth(api_key)
        .header("X-Blaxel-Workspace", workspace)
        .json(&json!({
            "targetType": "sandbox",
            "targetName": target_sandbox_id,
            "snapshotId": snapshot_id,
        }))
        .send()
        .await
        .map_err(request_error)?;
    checked(response).await?;
    Ok(target_sandbox_id.to_owned())
}

#[derive(Deserialize)]
struct SandboxResponse {
    status: String,
    #[serde(default)]
    metadata: SandboxMetadata,
}

#[derive(Default, Deserialize)]
struct SandboxMetadata {
    #[serde(default)]
    url: Option<String>,
}

#[allow(clippy::too_many_arguments)]
async fn wait_until_ready_at(
    client: &Client,
    base_url: Url,
    api_key: &str,
    workspace: &str,
    sandbox_id: &str,
    timeout: Duration,
    poll_interval: Duration,
) -> Result<ReadyBlaxelSandbox, BlaxelTransportError> {
    validate(api_key, "API key")?;
    validate(workspace, "workspace")?;
    validate(sandbox_id, "sandbox ID")?;
    let deadline = Instant::now() + timeout;
    loop {
        let response = client
            .get(sandbox_endpoint(base_url.clone(), sandbox_id)?)
            .bearer_auth(api_key)
            .header("X-Blaxel-Workspace", workspace)
            .send()
            .await
            .map_err(request_error)?;
        let sandbox = checked_json::<SandboxResponse>(response).await?;
        match sandbox.status.to_ascii_uppercase().as_str() {
            "DEPLOYED" => {
                let url = sandbox.metadata.url.ok_or_else(|| {
                    BlaxelTransportError::new("Blaxel sandbox response omitted metadata.url")
                })?;
                return Url::parse(&url)
                    .map(|sandbox_url| ReadyBlaxelSandbox { sandbox_url })
                    .map_err(|source| {
                        BlaxelTransportError::new(format!(
                            "Blaxel returned an invalid sandbox URL: {source}"
                        ))
                    });
            }
            "FAILED" | "TERMINATED" | "DEACTIVATED" => {
                return Err(BlaxelTransportError::new(format!(
                    "Blaxel sandbox entered terminal status {}",
                    sandbox.status
                )));
            }
            _ if Instant::now() >= deadline => {
                return Err(BlaxelTransportError {
                    message: "timed out waiting for Blaxel sandbox deployment".to_owned(),
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
    workspace: &str,
    sandbox_id: &str,
) -> Result<(), BlaxelTransportError> {
    validate(api_key, "API key")?;
    validate(workspace, "workspace")?;
    validate(sandbox_id, "sandbox ID")?;
    let response = client
        .delete(sandbox_endpoint(base_url, sandbox_id)?)
        .bearer_auth(api_key)
        .header("X-Blaxel-Workspace", workspace)
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
    Url::parse(API_URL).expect("Blaxel API URL is valid")
}

fn fork_endpoint(mut base_url: Url, source_sandbox_id: &str) -> Result<Url, BlaxelTransportError> {
    base_url
        .path_segments_mut()
        .map_err(|()| BlaxelTransportError::new("Blaxel API URL cannot be a base URL"))?
        .pop_if_empty()
        .extend(["sandboxes", source_sandbox_id, "fork"]);
    Ok(base_url)
}

fn sandbox_endpoint(mut base_url: Url, sandbox_id: &str) -> Result<Url, BlaxelTransportError> {
    base_url
        .path_segments_mut()
        .map_err(|()| BlaxelTransportError::new("Blaxel API URL cannot be a base URL"))?
        .pop_if_empty()
        .extend(["sandboxes", sandbox_id]);
    Ok(base_url)
}

fn validate(value: &str, field: &str) -> Result<(), BlaxelTransportError> {
    if value.is_empty() || value != value.trim() {
        return Err(BlaxelTransportError::new(format!(
            "Blaxel {field} must not be blank or have surrounding whitespace"
        )));
    }
    Ok(())
}

async fn checked(response: Response) -> Result<(), BlaxelTransportError> {
    if response.status().is_success() {
        return Ok(());
    }
    Err(response_error(response).await)
}

async fn checked_json<T: serde::de::DeserializeOwned>(
    response: Response,
) -> Result<T, BlaxelTransportError> {
    if !response.status().is_success() {
        return Err(response_error(response).await);
    }
    response.json().await.map_err(request_error)
}

async fn response_error(response: Response) -> BlaxelTransportError {
    let status = response.status();
    let body = response.text().await.unwrap_or_default();
    BlaxelTransportError {
        message: format!("Blaxel API returned {status}: {body}"),
        retryable: status.is_server_error() || status.as_u16() == 429,
        disconnected: false,
    }
}

fn request_error(source: reqwest::Error) -> BlaxelTransportError {
    BlaxelTransportError {
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
    async fn forks_the_source_sandbox_at_the_requested_snapshot() {
        async fn create(headers: HeaderMap, Json(body): Json<Value>) -> Json<Value> {
            assert_eq!(headers["authorization"], "Bearer blaxel-secret");
            assert_eq!(headers["x-blaxel-workspace"], "workspace-1");
            assert_eq!(
                body,
                json!({
                    "targetType": "sandbox",
                    "targetName": "target-1",
                    "snapshotId": "snapshot-1",
                })
            );
            Json(json!({ "status": "DEPLOYING" }))
        }

        let base = serve(Router::new().route("/v0/sandboxes/source-1/fork", post(create))).await;
        let sandbox_id = create_from_snapshot_at(
            &Client::new(),
            base,
            "blaxel-secret",
            "workspace-1",
            "source-1",
            "snapshot-1",
            "target-1",
        )
        .await
        .unwrap();
        assert_eq!(sandbox_id, "target-1");
    }

    #[tokio::test]
    async fn resolves_the_ready_sandbox_url_and_terminates() {
        async fn get(headers: HeaderMap) -> Json<Value> {
            assert_eq!(headers["authorization"], "Bearer blaxel-secret");
            assert_eq!(headers["x-blaxel-workspace"], "workspace-1");
            Json(json!({
                "status": "DEPLOYED",
                "metadata": {"url": "https://sandbox.example/"}
            }))
        }
        async fn delete(headers: HeaderMap) -> StatusCode {
            assert_eq!(headers["authorization"], "Bearer blaxel-secret");
            StatusCode::OK
        }
        let base = serve(Router::new().route(
            "/v0/sandboxes/sandbox-1",
            axum::routing::get(get).delete(delete),
        ))
        .await;
        let ready = wait_until_ready_at(
            &Client::new(),
            base.clone(),
            "blaxel-secret",
            "workspace-1",
            "sandbox-1",
            Duration::from_secs(1),
            Duration::from_millis(1),
        )
        .await
        .unwrap();
        assert_eq!(ready.sandbox_url.as_str(), "https://sandbox.example/");
        terminate_at(
            &Client::new(),
            base,
            "blaxel-secret",
            "workspace-1",
            "sandbox-1",
        )
        .await
        .unwrap();
    }

    async fn serve(app: Router) -> Url {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        Url::parse(&format!("http://{address}/v0/")).unwrap()
    }
}
