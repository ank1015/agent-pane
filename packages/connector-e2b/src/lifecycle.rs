use reqwest::{Client, Response, Url};
use serde::Deserialize;
use serde_json::json;

use crate::E2bTransportError;

const API_URL: &str = "https://api.e2b.app/";

/// Creates an E2B sandbox from a saved snapshot and returns its sandbox ID.
pub async fn create_from_snapshot(
    api_key: &str,
    snapshot_id: &str,
) -> Result<String, E2bTransportError> {
    create_from_snapshot_details(api_key, snapshot_id)
        .await
        .map(|sandbox| sandbox.sandbox_id)
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CreatedE2bSandbox {
    pub sandbox_id: String,
    pub envd_access_token: String,
    pub sandbox_domain: Option<String>,
}

/// Creates a secure E2B sandbox and retains the connection token returned once at creation.
pub async fn create_from_snapshot_details(
    api_key: &str,
    snapshot_id: &str,
) -> Result<CreatedE2bSandbox, E2bTransportError> {
    create_from_snapshot_at(&Client::new(), api_url(), api_key, snapshot_id).await
}

/// Permanently terminates an E2B sandbox. A missing sandbox is treated as terminated.
pub async fn terminate(api_key: &str, sandbox_id: &str) -> Result<(), E2bTransportError> {
    terminate_at(&Client::new(), api_url(), api_key, sandbox_id).await
}

async fn create_from_snapshot_at(
    client: &Client,
    base_url: Url,
    api_key: &str,
    snapshot_id: &str,
) -> Result<CreatedE2bSandbox, E2bTransportError> {
    validate(api_key, "API key")?;
    validate(snapshot_id, "snapshot ID")?;
    let response = client
        .post(endpoint(base_url, "sandboxes")?)
        .header("X-API-Key", api_key)
        .json(&json!({ "templateID": snapshot_id, "secure": true }))
        .send()
        .await
        .map_err(request_error)?;
    let response = checked_json::<CreateSandboxResponse>(response).await?;
    Ok(CreatedE2bSandbox {
        sandbox_id: response.sandbox_id,
        envd_access_token: response.envd_access_token,
        sandbox_domain: response.domain,
    })
}

#[derive(Deserialize)]
struct CreateSandboxResponse {
    #[serde(rename = "sandboxID")]
    sandbox_id: String,
    #[serde(rename = "envdAccessToken")]
    envd_access_token: String,
    #[serde(default)]
    domain: Option<String>,
}

async fn terminate_at(
    client: &Client,
    base_url: Url,
    api_key: &str,
    sandbox_id: &str,
) -> Result<(), E2bTransportError> {
    validate(api_key, "API key")?;
    validate(sandbox_id, "sandbox ID")?;
    let response = client
        .delete(endpoint(base_url, &format!("sandboxes/{sandbox_id}"))?)
        .header("X-API-Key", api_key)
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
    Url::parse(API_URL).expect("E2B API URL is valid")
}

fn endpoint(mut base_url: Url, segment: &str) -> Result<Url, E2bTransportError> {
    base_url
        .path_segments_mut()
        .map_err(|()| E2bTransportError::new("E2B API URL cannot be a base URL"))?
        .pop_if_empty()
        .extend(segment.split('/'));
    Ok(base_url)
}

fn validate(value: &str, field: &str) -> Result<(), E2bTransportError> {
    if value.is_empty() || value != value.trim() {
        return Err(E2bTransportError::new(format!(
            "E2B {field} must not be blank or have surrounding whitespace"
        )));
    }
    Ok(())
}

async fn checked_json<T: serde::de::DeserializeOwned>(
    response: Response,
) -> Result<T, E2bTransportError> {
    if !response.status().is_success() {
        return Err(response_error(response).await);
    }
    response.json().await.map_err(request_error)
}

async fn response_error(response: Response) -> E2bTransportError {
    let status = response.status();
    let body = response.text().await.unwrap_or_default();
    E2bTransportError {
        message: format!("E2B API returned {status}: {body}"),
        retryable: status.is_server_error() || status.as_u16() == 429,
        disconnected: false,
    }
}

fn request_error(source: reqwest::Error) -> E2bTransportError {
    E2bTransportError {
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
            assert_eq!(headers["x-api-key"], "e2b-secret");
            assert_eq!(body, json!({ "templateID": "snapshot-1", "secure": true }));
            Json(json!({
                "sandboxID": "sandbox-1",
                "envdAccessToken": "access-1",
                "domain": "e2b.app"
            }))
        }

        let base = serve(Router::new().route("/sandboxes", post(create))).await;
        let sandbox_id = create_from_snapshot_at(&Client::new(), base, "e2b-secret", "snapshot-1")
            .await
            .unwrap();
        assert_eq!(sandbox_id.sandbox_id, "sandbox-1");
        assert_eq!(sandbox_id.envd_access_token, "access-1");
    }

    #[tokio::test]
    async fn terminates_the_created_sandbox() {
        async fn delete(headers: HeaderMap) -> StatusCode {
            assert_eq!(headers["x-api-key"], "e2b-secret");
            StatusCode::NO_CONTENT
        }
        let base =
            serve(Router::new().route("/sandboxes/sandbox-1", axum::routing::delete(delete))).await;
        terminate_at(&Client::new(), base, "e2b-secret", "sandbox-1")
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
