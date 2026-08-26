use std::time::{Duration, Instant};

use reqwest::{Client, Response, Url, header};
use serde::Deserialize;
use serde_json::json;

use crate::BlaxelTransportError;

const API_URL: &str = "https://api.blaxel.ai/v0/";
const API_VERSION: &str = "2026-04-16";

/// Resolves the only Blaxel workspace available to an API key.
pub async fn resolve_workspace(api_key: &str) -> Result<String, BlaxelTransportError> {
    let client = client()?;
    resolve_workspace_at(&client, api_url(), api_key).await
}

/// Creates a Blaxel sandbox with the provider's current default image.
pub async fn create(
    api_key: &str,
    workspace: &str,
    sandbox_id: &str,
) -> Result<String, BlaxelTransportError> {
    let client = client()?;
    create_at(&client, api_url(), api_key, workspace, sandbox_id).await
}

/// Forks a Blaxel sandbox from a saved snapshot and returns the target sandbox ID.
pub async fn create_from_snapshot(
    api_key: &str,
    workspace: &str,
    source_sandbox_id: &str,
    snapshot_id: &str,
    target_sandbox_id: &str,
) -> Result<String, BlaxelTransportError> {
    let client = client()?;
    create_from_snapshot_at(
        &client,
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
    let client = client()?;
    wait_until_ready_at(
        &client,
        api_url(),
        api_key,
        workspace,
        sandbox_id,
        timeout,
        poll_interval,
    )
    .await
}

/// Creates a restorable snapshot and waits until Blaxel reports it ready.
pub async fn create_snapshot(
    api_key: &str,
    workspace: &str,
    sandbox_id: &str,
    snapshot_name: &str,
) -> Result<String, BlaxelTransportError> {
    let client = client()?;
    create_snapshot_at(
        &client,
        api_url(),
        api_key,
        workspace,
        sandbox_id,
        snapshot_name,
        Duration::from_secs(300),
        Duration::from_millis(500),
    )
    .await
}

/// Deletes a Blaxel sandbox. A missing sandbox is treated as deleted.
pub async fn terminate(
    api_key: &str,
    workspace: &str,
    sandbox_id: &str,
) -> Result<(), BlaxelTransportError> {
    let client = client()?;
    terminate_at(&client, api_url(), api_key, workspace, sandbox_id).await
}

#[derive(Deserialize)]
struct WorkspaceResponse {
    name: String,
}

async fn resolve_workspace_at(
    client: &Client,
    base_url: Url,
    api_key: &str,
) -> Result<String, BlaxelTransportError> {
    validate(api_key, "API key")?;
    let response = client
        .get(endpoint(base_url, &["workspaces"])?)
        .bearer_auth(api_key)
        .send()
        .await
        .map_err(request_error)?;
    let workspaces = checked_json::<Vec<WorkspaceResponse>>(response).await?;
    match workspaces.as_slice() {
        [workspace] => Ok(workspace.name.clone()),
        [] => Err(BlaxelTransportError::new(
            "Blaxel API key does not have access to a workspace",
        )),
        _ => Err(BlaxelTransportError::new(
            "Blaxel API key has access to multiple workspaces; set account config.workspace",
        )),
    }
}

async fn create_at(
    client: &Client,
    base_url: Url,
    api_key: &str,
    workspace: &str,
    sandbox_id: &str,
) -> Result<String, BlaxelTransportError> {
    validate(api_key, "API key")?;
    validate(workspace, "workspace")?;
    validate_resource_name(sandbox_id, "sandbox ID")?;
    let response = client
        .post(endpoint(base_url, &["sandboxes"])?)
        .bearer_auth(api_key)
        .header("X-Blaxel-Workspace", workspace)
        .json(&json!({
            "metadata": {"name": sandbox_id},
            "spec": {"region": "auto"},
        }))
        .send()
        .await
        .map_err(request_error)?;
    checked(response).await?;
    Ok(sandbox_id.to_owned())
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
    validate_resource_name(source_sandbox_id, "source sandbox ID")?;
    validate(snapshot_id, "snapshot ID")?;
    validate_resource_name(target_sandbox_id, "target sandbox ID")?;
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

#[derive(Clone, Deserialize)]
struct SnapshotResponse {
    id: String,
    status: String,
}

#[allow(clippy::too_many_arguments)]
async fn create_snapshot_at(
    client: &Client,
    base_url: Url,
    api_key: &str,
    workspace: &str,
    sandbox_id: &str,
    snapshot_name: &str,
    timeout: Duration,
    poll_interval: Duration,
) -> Result<String, BlaxelTransportError> {
    validate(api_key, "API key")?;
    validate(workspace, "workspace")?;
    validate_resource_name(sandbox_id, "sandbox ID")?;
    validate_resource_name(snapshot_name, "snapshot name")?;
    let snapshots_url = snapshots_endpoint(base_url, sandbox_id)?;
    let response = client
        .post(snapshots_url.clone())
        .bearer_auth(api_key)
        .header("X-Blaxel-Workspace", workspace)
        .json(&json!({"name": snapshot_name}))
        .send()
        .await
        .map_err(request_error)?;
    let created = checked_json::<SnapshotResponse>(response).await?;
    wait_for_snapshot(
        client,
        snapshots_url,
        api_key,
        workspace,
        created,
        timeout,
        poll_interval,
    )
    .await
}

#[allow(clippy::too_many_arguments)]
async fn wait_for_snapshot(
    client: &Client,
    snapshots_url: Url,
    api_key: &str,
    workspace: &str,
    created: SnapshotResponse,
    timeout: Duration,
    poll_interval: Duration,
) -> Result<String, BlaxelTransportError> {
    let deadline = Instant::now() + timeout;
    let snapshot_id = created.id;
    let mut status = created.status;
    loop {
        match status.to_ascii_lowercase().as_str() {
            "ready" => return Ok(snapshot_id),
            "failed" => {
                return Err(BlaxelTransportError::new(format!(
                    "Blaxel snapshot {snapshot_id} failed"
                )));
            }
            _ if Instant::now() >= deadline => {
                return Err(BlaxelTransportError {
                    message: format!("timed out waiting for Blaxel snapshot {snapshot_id}"),
                    retryable: true,
                    disconnected: false,
                });
            }
            _ => tokio::time::sleep(poll_interval).await,
        }
        let response = client
            .get(snapshots_url.clone())
            .bearer_auth(api_key)
            .header("X-Blaxel-Workspace", workspace)
            .send()
            .await
            .map_err(request_error)?;
        let snapshots = checked_json::<Vec<SnapshotResponse>>(response).await?;
        status = snapshots
            .into_iter()
            .find(|snapshot| snapshot.id == snapshot_id)
            .ok_or_else(|| {
                BlaxelTransportError::new(format!(
                    "Blaxel snapshot {snapshot_id} disappeared while being created"
                ))
            })?
            .status;
    }
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
    validate_resource_name(sandbox_id, "sandbox ID")?;
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
    validate_resource_name(sandbox_id, "sandbox ID")?;
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

fn client() -> Result<Client, BlaxelTransportError> {
    let mut headers = header::HeaderMap::new();
    headers.insert(
        header::ACCEPT,
        header::HeaderValue::from_static("application/json, text/plain, */*"),
    );
    headers.insert(
        header::HeaderName::from_static("blaxel-version"),
        header::HeaderValue::from_static(API_VERSION),
    );
    Client::builder()
        .user_agent("agent-pane-connector-blaxel/1.0")
        .default_headers(headers)
        .build()
        .map_err(request_error)
}

fn endpoint(mut base_url: Url, segments: &[&str]) -> Result<Url, BlaxelTransportError> {
    base_url
        .path_segments_mut()
        .map_err(|()| BlaxelTransportError::new("Blaxel API URL cannot be a base URL"))?
        .pop_if_empty()
        .extend(segments);
    Ok(base_url)
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

fn snapshots_endpoint(base_url: Url, sandbox_id: &str) -> Result<Url, BlaxelTransportError> {
    endpoint(base_url, &["sandboxes", sandbox_id, "snapshots"])
}

fn validate(value: &str, field: &str) -> Result<(), BlaxelTransportError> {
    if value.is_empty() || value != value.trim() {
        return Err(BlaxelTransportError::new(format!(
            "Blaxel {field} must not be blank or have surrounding whitespace"
        )));
    }
    Ok(())
}

fn validate_resource_name(value: &str, field: &str) -> Result<(), BlaxelTransportError> {
    validate(value, field)?;
    if value.chars().count() > 49 {
        return Err(BlaxelTransportError::new(format!(
            "Blaxel {field} must be at most 49 characters"
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
#[path = "lifecycle_tests.rs"]
mod tests;
