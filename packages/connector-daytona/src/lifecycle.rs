use std::time::{Duration, Instant};

use reqwest::{Client, Response, Url};
use serde::Deserialize;
use serde_json::{Value, json};

use crate::DaytonaTransportError;

const API_URL: &str = "https://app.daytona.io/api/";
const DEFAULT_AUTO_STOP_INTERVAL_MINUTES: u64 = 15;

/// Creates a Daytona container sandbox from Daytona's default snapshot.
pub async fn create(api_key: &str, name: Option<&str>) -> Result<String, DaytonaTransportError> {
    create_at(&Client::new(), api_url(), api_key, name).await
}

/// Creates a Daytona sandbox from a saved snapshot and returns its sandbox ID.
pub async fn create_from_snapshot(
    api_key: &str,
    snapshot: &str,
) -> Result<String, DaytonaTransportError> {
    create_from_snapshot_at(&Client::new(), api_url(), api_key, snapshot).await
}

#[derive(Clone, Debug)]
pub struct ReadyDaytonaSandbox {
    pub toolbox_url: Option<Url>,
    pub network_block_all: bool,
}

/// Waits until Daytona reports the sandbox as started.
pub async fn wait_until_ready(
    api_key: &str,
    sandbox_id: &str,
    timeout: Duration,
    poll_interval: Duration,
) -> Result<ReadyDaytonaSandbox, DaytonaTransportError> {
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

/// Starts a stopped, paused, or archived Daytona sandbox and waits for it to be ready.
pub async fn ensure_started(
    api_key: &str,
    sandbox_id: &str,
    timeout: Duration,
    poll_interval: Duration,
) -> Result<ReadyDaytonaSandbox, DaytonaTransportError> {
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

/// Creates a cold filesystem snapshot and returns its Daytona snapshot name.
pub async fn create_snapshot(
    api_key: &str,
    sandbox_id: &str,
    snapshot_name: &str,
) -> Result<String, DaytonaTransportError> {
    create_snapshot_at(
        &Client::new(),
        api_url(),
        api_key,
        sandbox_id,
        snapshot_name,
        Duration::from_secs(300),
        Duration::from_millis(500),
    )
    .await
}

/// Stops a Daytona sandbox while preserving its filesystem.
pub async fn stop(api_key: &str, sandbox_id: &str) -> Result<(), DaytonaTransportError> {
    stop_at(&Client::new(), api_url(), api_key, sandbox_id).await
}

/// Deletes a Daytona sandbox. A missing sandbox is treated as deleted.
pub async fn terminate(api_key: &str, sandbox_id: &str) -> Result<(), DaytonaTransportError> {
    terminate_at(&Client::new(), api_url(), api_key, sandbox_id).await
}

async fn create_at(
    client: &Client,
    base_url: Url,
    api_key: &str,
    name: Option<&str>,
) -> Result<String, DaytonaTransportError> {
    validate(api_key, "API key")?;
    if let Some(name) = name {
        validate(name, "sandbox name")?;
    }
    let mut body = serde_json::Map::from_iter([(
        "autoStopInterval".to_owned(),
        Value::from(DEFAULT_AUTO_STOP_INTERVAL_MINUTES),
    )]);
    if let Some(name) = name {
        body.insert("name".to_owned(), Value::from(name));
    }
    let response = client
        .post(endpoint(base_url, "sandbox")?)
        .bearer_auth(api_key)
        .json(&body)
        .send()
        .await
        .map_err(request_error)?;
    checked_json::<CreateSandboxResponse>(response)
        .await
        .map(|response| response.id)
}

async fn create_from_snapshot_at(
    client: &Client,
    base_url: Url,
    api_key: &str,
    snapshot: &str,
) -> Result<String, DaytonaTransportError> {
    validate(api_key, "API key")?;
    validate(snapshot, "snapshot reference")?;
    let response = client
        .post(endpoint(base_url, "sandbox")?)
        .bearer_auth(api_key)
        .json(&json!({
            "snapshot": snapshot,
            "autoStopInterval": DEFAULT_AUTO_STOP_INTERVAL_MINUTES,
        }))
        .send()
        .await
        .map_err(request_error)?;
    checked_json::<CreateSandboxResponse>(response)
        .await
        .map(|response| response.id)
}

#[derive(Deserialize)]
struct CreateSandboxResponse {
    id: String,
}

#[derive(Deserialize)]
struct SandboxResponse {
    state: String,
    #[serde(default, alias = "toolboxProxyUrl")]
    toolbox_proxy_url: Option<String>,
    #[serde(default, alias = "networkBlockAll")]
    network_block_all: bool,
}

async fn ensure_started_at(
    client: &Client,
    base_url: Url,
    api_key: &str,
    sandbox_id: &str,
    timeout: Duration,
    poll_interval: Duration,
) -> Result<ReadyDaytonaSandbox, DaytonaTransportError> {
    validate(api_key, "API key")?;
    validate(sandbox_id, "sandbox ID")?;
    let deadline = Instant::now() + timeout;
    loop {
        let sandbox = get_sandbox(client, base_url.clone(), api_key, sandbox_id).await?;
        match normalized_state(&sandbox).as_str() {
            "started" | "running" => return ready_sandbox(sandbox, sandbox_id),
            "stopped" | "paused" | "archived" => {
                let response = client
                    .post(endpoint(
                        base_url.clone(),
                        &format!("sandbox/{sandbox_id}/start"),
                    )?)
                    .bearer_auth(api_key)
                    .send()
                    .await
                    .map_err(request_error)?;
                if !response.status().is_success() && response.status().as_u16() != 409 {
                    return Err(response_error(response).await);
                }
            }
            "error" | "destroyed" | "deleted" | "build failed" | "build_failed" => {
                return Err(terminal_state_error(&sandbox.state));
            }
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
    snapshot_name: &str,
    timeout: Duration,
    poll_interval: Duration,
) -> Result<String, DaytonaTransportError> {
    validate(api_key, "API key")?;
    validate(sandbox_id, "sandbox ID")?;
    validate(snapshot_name, "snapshot name")?;
    ensure_started_at(
        client,
        base_url.clone(),
        api_key,
        sandbox_id,
        timeout,
        poll_interval,
    )
    .await?;
    stop_at(client, base_url.clone(), api_key, sandbox_id).await?;
    wait_for_state(
        client,
        base_url.clone(),
        api_key,
        sandbox_id,
        "stopped",
        timeout,
        poll_interval,
    )
    .await?;
    let snapshot = client
        .post(endpoint(
            base_url.clone(),
            &format!("sandbox/{sandbox_id}/snapshot"),
        )?)
        .bearer_auth(api_key)
        .json(&json!({
            "name": snapshot_name,
            "includeMemory": false,
        }))
        .send()
        .await
        .map_err(request_error)?;
    if let Err(error) = checked(snapshot).await {
        let _ = ensure_started_at(
            client,
            base_url,
            api_key,
            sandbox_id,
            timeout,
            poll_interval,
        )
        .await;
        return Err(error);
    }
    ensure_started_at(
        client,
        base_url,
        api_key,
        sandbox_id,
        timeout,
        poll_interval,
    )
    .await?;
    Ok(snapshot_name.to_owned())
}

async fn stop_at(
    client: &Client,
    base_url: Url,
    api_key: &str,
    sandbox_id: &str,
) -> Result<(), DaytonaTransportError> {
    validate(api_key, "API key")?;
    validate(sandbox_id, "sandbox ID")?;
    let response = client
        .post(endpoint(base_url, &format!("sandbox/{sandbox_id}/stop"))?)
        .bearer_auth(api_key)
        .send()
        .await
        .map_err(request_error)?;
    checked(response).await
}

#[allow(clippy::too_many_arguments)]
async fn wait_until_ready_at(
    client: &Client,
    base_url: Url,
    api_key: &str,
    sandbox_id: &str,
    timeout: Duration,
    poll_interval: Duration,
) -> Result<ReadyDaytonaSandbox, DaytonaTransportError> {
    validate(api_key, "API key")?;
    validate(sandbox_id, "sandbox ID")?;
    let deadline = Instant::now() + timeout;
    loop {
        let sandbox = get_sandbox(client, base_url.clone(), api_key, sandbox_id).await?;
        match normalized_state(&sandbox).as_str() {
            "started" | "running" => return ready_sandbox(sandbox, sandbox_id),
            "error" | "destroyed" | "deleted" | "build failed" | "build_failed" => {
                return Err(terminal_state_error(&sandbox.state));
            }
            _ if Instant::now() >= deadline => {
                return Err(timeout_error("start"));
            }
            _ => tokio::time::sleep(poll_interval).await,
        }
    }
}

#[allow(clippy::too_many_arguments)]
async fn wait_for_state(
    client: &Client,
    base_url: Url,
    api_key: &str,
    sandbox_id: &str,
    expected: &str,
    timeout: Duration,
    poll_interval: Duration,
) -> Result<(), DaytonaTransportError> {
    let deadline = Instant::now() + timeout;
    loop {
        let sandbox = get_sandbox(client, base_url.clone(), api_key, sandbox_id).await?;
        let state = normalized_state(&sandbox);
        if state == expected {
            return Ok(());
        }
        if matches!(
            state.as_str(),
            "error" | "destroyed" | "deleted" | "build failed" | "build_failed"
        ) {
            return Err(terminal_state_error(&sandbox.state));
        }
        if Instant::now() >= deadline {
            return Err(timeout_error(expected));
        }
        tokio::time::sleep(poll_interval).await;
    }
}

async fn get_sandbox(
    client: &Client,
    base_url: Url,
    api_key: &str,
    sandbox_id: &str,
) -> Result<SandboxResponse, DaytonaTransportError> {
    let response = client
        .get(endpoint(base_url, &format!("sandbox/{sandbox_id}"))?)
        .bearer_auth(api_key)
        .send()
        .await
        .map_err(request_error)?;
    checked_json(response).await
}

fn normalized_state(sandbox: &SandboxResponse) -> String {
    sandbox.state.to_ascii_lowercase()
}

fn ready_sandbox(
    sandbox: SandboxResponse,
    sandbox_id: &str,
) -> Result<ReadyDaytonaSandbox, DaytonaTransportError> {
    let toolbox_url = sandbox
        .toolbox_proxy_url
        .map(|value| toolbox_url(&value, sandbox_id))
        .transpose()?;
    Ok(ReadyDaytonaSandbox {
        toolbox_url,
        network_block_all: sandbox.network_block_all,
    })
}

fn terminal_state_error(state: &str) -> DaytonaTransportError {
    DaytonaTransportError::new(format!("Daytona sandbox entered terminal state {state}"))
}

fn timeout_error(action: &str) -> DaytonaTransportError {
    DaytonaTransportError {
        message: format!("timed out waiting for Daytona sandbox to {action}"),
        retryable: true,
        disconnected: false,
    }
}

async fn terminate_at(
    client: &Client,
    base_url: Url,
    api_key: &str,
    sandbox_id: &str,
) -> Result<(), DaytonaTransportError> {
    validate(api_key, "API key")?;
    validate(sandbox_id, "sandbox ID")?;
    let response = client
        .delete(endpoint(base_url, &format!("sandbox/{sandbox_id}"))?)
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
    Url::parse(API_URL).expect("Daytona API URL is valid")
}

fn endpoint(mut base_url: Url, segment: &str) -> Result<Url, DaytonaTransportError> {
    base_url
        .path_segments_mut()
        .map_err(|()| DaytonaTransportError::new("Daytona API URL cannot be a base URL"))?
        .pop_if_empty()
        .extend(segment.split('/'));
    Ok(base_url)
}

fn toolbox_url(base_url: &str, sandbox_id: &str) -> Result<Url, DaytonaTransportError> {
    let mut url = Url::parse(base_url).map_err(request_error_from_url)?;
    url.path_segments_mut()
        .map_err(|()| DaytonaTransportError::new("Daytona Toolbox URL cannot be a base URL"))?
        .pop_if_empty()
        .push(sandbox_id)
        .push("");
    Ok(url)
}

fn validate(value: &str, field: &str) -> Result<(), DaytonaTransportError> {
    if value.is_empty() || value != value.trim() {
        return Err(DaytonaTransportError::new(format!(
            "Daytona {field} must not be blank or have surrounding whitespace"
        )));
    }
    Ok(())
}

async fn checked_json<T: serde::de::DeserializeOwned>(
    response: Response,
) -> Result<T, DaytonaTransportError> {
    if !response.status().is_success() {
        return Err(response_error(response).await);
    }
    response.json().await.map_err(request_error)
}

async fn checked(response: Response) -> Result<(), DaytonaTransportError> {
    if response.status().is_success() {
        Ok(())
    } else {
        Err(response_error(response).await)
    }
}

async fn response_error(response: Response) -> DaytonaTransportError {
    let status = response.status();
    let body = response.text().await.unwrap_or_default();
    DaytonaTransportError {
        message: format!("Daytona API returned {status}: {body}"),
        retryable: status.is_server_error() || status.as_u16() == 429,
        disconnected: false,
    }
}

fn request_error(source: reqwest::Error) -> DaytonaTransportError {
    DaytonaTransportError {
        message: source.to_string(),
        retryable: source.is_timeout() || source.is_connect(),
        disconnected: source.is_connect() || source.is_timeout() || source.is_body(),
    }
}

fn request_error_from_url(source: url::ParseError) -> DaytonaTransportError {
    DaytonaTransportError::new(format!("Daytona returned an invalid toolbox URL: {source}"))
}

#[cfg(test)]
#[path = "lifecycle_tests.rs"]
mod tests;
