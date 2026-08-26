use std::time::{Duration, Instant};

use reqwest::{Client, Response, Url};
use serde::Deserialize;
use serde_json::json;

use crate::DaytonaTransportError;

const API_URL: &str = "https://app.daytona.io/api/";

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

/// Deletes a Daytona sandbox. A missing sandbox is treated as deleted.
pub async fn terminate(api_key: &str, sandbox_id: &str) -> Result<(), DaytonaTransportError> {
    terminate_at(&Client::new(), api_url(), api_key, sandbox_id).await
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
        .json(&json!({ "snapshot": snapshot }))
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
        let response = client
            .get(endpoint(
                base_url.clone(),
                &format!("sandbox/{sandbox_id}"),
            )?)
            .bearer_auth(api_key)
            .send()
            .await
            .map_err(request_error)?;
        let sandbox = checked_json::<SandboxResponse>(response).await?;
        match sandbox.state.to_ascii_lowercase().as_str() {
            "started" | "running" => {
                let toolbox_url = sandbox
                    .toolbox_proxy_url
                    .map(|value| toolbox_url(&value, sandbox_id))
                    .transpose()?;
                return Ok(ReadyDaytonaSandbox {
                    toolbox_url,
                    network_block_all: sandbox.network_block_all,
                });
            }
            "error" | "destroyed" => {
                return Err(DaytonaTransportError::new(format!(
                    "Daytona sandbox entered terminal state {}",
                    sandbox.state
                )));
            }
            _ if Instant::now() >= deadline => {
                return Err(DaytonaTransportError {
                    message: "timed out waiting for Daytona sandbox to start".to_owned(),
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
mod tests {
    use axum::{Json, Router, http::HeaderMap, http::StatusCode, routing::post};
    use serde_json::{Value, json};
    use tokio::net::TcpListener;

    use super::*;

    #[tokio::test]
    async fn creates_a_sandbox_from_the_snapshot_reference() {
        async fn create(headers: HeaderMap, Json(body): Json<Value>) -> Json<Value> {
            assert_eq!(headers["authorization"], "Bearer daytona-secret");
            assert_eq!(body, json!({ "snapshot": "snapshot-1" }));
            Json(json!({ "id": "sandbox-1" }))
        }

        let base = serve(Router::new().route("/api/sandbox", post(create))).await;
        let sandbox_id =
            create_from_snapshot_at(&Client::new(), base, "daytona-secret", "snapshot-1")
                .await
                .unwrap();
        assert_eq!(sandbox_id, "sandbox-1");
    }

    #[tokio::test]
    async fn resolves_the_ready_toolbox_and_terminates() {
        async fn get(headers: HeaderMap) -> Json<Value> {
            assert_eq!(headers["authorization"], "Bearer daytona-secret");
            Json(json!({
                "state": "started",
                "toolboxProxyUrl": "https://toolbox.example/toolbox",
                "networkBlockAll": true
            }))
        }
        async fn delete(headers: HeaderMap) -> StatusCode {
            assert_eq!(headers["authorization"], "Bearer daytona-secret");
            StatusCode::NO_CONTENT
        }
        let base = serve(Router::new().route(
            "/api/sandbox/sandbox-1",
            axum::routing::get(get).delete(delete),
        ))
        .await;
        let ready = wait_until_ready_at(
            &Client::new(),
            base.clone(),
            "daytona-secret",
            "sandbox-1",
            Duration::from_secs(1),
            Duration::from_millis(1),
        )
        .await
        .unwrap();
        assert!(ready.network_block_all);
        assert_eq!(
            ready.toolbox_url.unwrap().as_str(),
            "https://toolbox.example/toolbox/sandbox-1/"
        );
        terminate_at(&Client::new(), base, "daytona-secret", "sandbox-1")
            .await
            .unwrap();
    }

    async fn serve(app: Router) -> Url {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        Url::parse(&format!("http://{address}/api/")).unwrap()
    }
}
