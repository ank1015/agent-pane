use std::time::{Duration, Instant};

use reqwest::{Client, Response, Url};
use serde::Deserialize;
use serde_json::json;

use crate::TensorlakeTransportError;

const API_URL: &str = "https://api.tensorlake.ai/";
const DEFAULT_IDLE_TIMEOUT_SECONDS: u64 = 600;

/// Creates a named Tensorlake sandbox using the default managed image.
pub async fn create(api_key: &str, name: &str) -> Result<String, TensorlakeTransportError> {
    create_at(&Client::new(), api_url(), api_key, name).await
}

/// Creates a Tensorlake sandbox from a saved snapshot and returns its sandbox ID.
pub async fn create_from_snapshot(
    api_key: &str,
    snapshot_id: &str,
    name: &str,
) -> Result<String, TensorlakeTransportError> {
    create_from_snapshot_at(&Client::new(), api_url(), api_key, snapshot_id, name).await
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

/// Resumes a suspended Tensorlake sandbox and waits until its management API is ready.
pub async fn ensure_started(
    api_key: &str,
    sandbox_id: &str,
    timeout: Duration,
    poll_interval: Duration,
) -> Result<ReadyTensorlakeSandbox, TensorlakeTransportError> {
    ensure_started_at(
        &Client::new(),
        api_url(),
        api_key,
        sandbox_id,
        timeout,
        poll_interval,
    )
    .await
}

/// Creates a reusable filesystem snapshot and returns its Tensorlake snapshot ID.
pub async fn create_snapshot(
    api_key: &str,
    sandbox_id: &str,
) -> Result<String, TensorlakeTransportError> {
    create_snapshot_at(
        &Client::new(),
        api_url(),
        api_key,
        sandbox_id,
        Duration::from_secs(300),
        Duration::from_millis(500),
    )
    .await
}

/// Terminates a Tensorlake sandbox. A missing sandbox is treated as terminated.
pub async fn terminate(api_key: &str, sandbox_id: &str) -> Result<(), TensorlakeTransportError> {
    terminate_at(&Client::new(), api_url(), api_key, sandbox_id).await
}

async fn create_at(
    client: &Client,
    base_url: Url,
    api_key: &str,
    name: &str,
) -> Result<String, TensorlakeTransportError> {
    create_with_body(
        client,
        base_url,
        api_key,
        json!({
            "name": name,
            "timeout_secs": DEFAULT_IDLE_TIMEOUT_SECONDS,
        }),
    )
    .await
}

async fn create_from_snapshot_at(
    client: &Client,
    base_url: Url,
    api_key: &str,
    snapshot_id: &str,
    name: &str,
) -> Result<String, TensorlakeTransportError> {
    validate(snapshot_id, "snapshot ID")?;
    create_with_body(
        client,
        base_url,
        api_key,
        json!({
            "snapshot_id": snapshot_id,
            "name": name,
            "timeout_secs": DEFAULT_IDLE_TIMEOUT_SECONDS,
        }),
    )
    .await
}

async fn create_with_body(
    client: &Client,
    base_url: Url,
    api_key: &str,
    body: serde_json::Value,
) -> Result<String, TensorlakeTransportError> {
    validate(api_key, "API key")?;
    let name = body
        .get("name")
        .and_then(serde_json::Value::as_str)
        .unwrap_or_default();
    validate(name, "sandbox name")?;
    let response = client
        .post(endpoint(base_url, "sandboxes")?)
        .bearer_auth(api_key)
        .json(&body)
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

#[derive(Deserialize)]
struct CreateSnapshotResponse {
    snapshot_id: String,
    status: String,
}

#[derive(Deserialize)]
struct SnapshotResponse {
    status: String,
    #[serde(default)]
    error: Option<String>,
}

async fn ensure_started_at(
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
        let sandbox = get_sandbox(client, base_url.clone(), api_key, sandbox_id).await?;
        match sandbox.status.as_str() {
            "running" => return ready_sandbox(sandbox),
            "suspended" => {
                let response = client
                    .post(endpoint(
                        base_url.clone(),
                        &format!("sandboxes/{sandbox_id}/resume"),
                    )?)
                    .bearer_auth(api_key)
                    .send()
                    .await
                    .map_err(request_error)?;
                if !response.status().is_success() && response.status().as_u16() != 409 {
                    return Err(response_error(response).await);
                }
            }
            "terminated" => return Err(terminated_error(&sandbox)),
            _ => {}
        }
        if Instant::now() >= deadline {
            return Err(timeout_error("start"));
        }
        tokio::time::sleep(poll_interval).await;
    }
}

#[allow(clippy::too_many_arguments)]
async fn create_snapshot_at(
    client: &Client,
    base_url: Url,
    api_key: &str,
    sandbox_id: &str,
    timeout: Duration,
    poll_interval: Duration,
) -> Result<String, TensorlakeTransportError> {
    validate(api_key, "API key")?;
    validate(sandbox_id, "sandbox ID")?;
    ensure_started_at(
        client,
        base_url.clone(),
        api_key,
        sandbox_id,
        timeout,
        poll_interval,
    )
    .await?;
    let response = client
        .post(endpoint(
            base_url.clone(),
            &format!("sandboxes/{sandbox_id}/snapshot"),
        )?)
        .bearer_auth(api_key)
        .json(&json!({"snapshot_type": "filesystem"}))
        .send()
        .await
        .map_err(request_error)?;
    let created = checked_json::<CreateSnapshotResponse>(response).await?;
    validate(&created.snapshot_id, "snapshot ID")?;
    wait_for_snapshot(
        client,
        base_url,
        api_key,
        &created.snapshot_id,
        &created.status,
        timeout,
        poll_interval,
    )
    .await?;
    Ok(created.snapshot_id)
}

#[allow(clippy::too_many_arguments)]
async fn wait_for_snapshot(
    client: &Client,
    base_url: Url,
    api_key: &str,
    snapshot_id: &str,
    initial_status: &str,
    timeout: Duration,
    poll_interval: Duration,
) -> Result<(), TensorlakeTransportError> {
    if snapshot_ready(initial_status) {
        return Ok(());
    }
    let deadline = Instant::now() + timeout;
    loop {
        let response = client
            .get(endpoint(
                base_url.clone(),
                &format!("snapshots/{snapshot_id}"),
            )?)
            .bearer_auth(api_key)
            .send()
            .await
            .map_err(request_error)?;
        let snapshot = checked_json::<SnapshotResponse>(response).await?;
        if snapshot_ready(&snapshot.status) {
            return Ok(());
        }
        if snapshot.status == "failed" {
            return Err(TensorlakeTransportError::new(format!(
                "Tensorlake snapshot failed{}",
                snapshot
                    .error
                    .as_deref()
                    .map(|error| format!(": {error}"))
                    .unwrap_or_default()
            )));
        }
        if Instant::now() >= deadline {
            return Err(timeout_error("snapshot"));
        }
        tokio::time::sleep(poll_interval).await;
    }
}

fn snapshot_ready(status: &str) -> bool {
    matches!(status, "local_ready" | "completed")
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
        let sandbox = get_sandbox(client, base_url.clone(), api_key, sandbox_id).await?;
        match sandbox.status.as_str() {
            "running" => return ready_sandbox(sandbox),
            "terminated" => return Err(terminated_error(&sandbox)),
            _ if Instant::now() >= deadline => return Err(timeout_error("run")),
            _ => tokio::time::sleep(poll_interval).await,
        }
    }
}

async fn get_sandbox(
    client: &Client,
    base_url: Url,
    api_key: &str,
    sandbox_id: &str,
) -> Result<SandboxResponse, TensorlakeTransportError> {
    let response = client
        .get(endpoint(base_url, &format!("sandboxes/{sandbox_id}"))?)
        .bearer_auth(api_key)
        .send()
        .await
        .map_err(request_error)?;
    checked_json(response).await
}

fn ready_sandbox(
    sandbox: SandboxResponse,
) -> Result<ReadyTensorlakeSandbox, TensorlakeTransportError> {
    let url = sandbox.sandbox_url.ok_or_else(|| {
        TensorlakeTransportError::new("Tensorlake running sandbox response omitted sandbox_url")
    })?;
    Url::parse(&url)
        .map(|sandbox_url| ReadyTensorlakeSandbox { sandbox_url })
        .map_err(|source| {
            TensorlakeTransportError::new(format!(
                "Tensorlake returned an invalid sandbox URL: {source}"
            ))
        })
}

fn terminated_error(sandbox: &SandboxResponse) -> TensorlakeTransportError {
    TensorlakeTransportError::new(format!(
        "Tensorlake sandbox terminated during provisioning{}",
        sandbox
            .outcome
            .as_deref()
            .map(|outcome| format!(": {outcome}"))
            .unwrap_or_default()
    ))
}

fn timeout_error(action: &str) -> TensorlakeTransportError {
    TensorlakeTransportError {
        message: format!("timed out waiting for Tensorlake sandbox to {action}"),
        retryable: true,
        disconnected: false,
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
#[path = "lifecycle_tests.rs"]
mod tests;
