use std::path::Path;

use anyhow::Context as _;
use execution_api::{ClaimRegisteredHostRequest, ClaimRegisteredHostResponse};
use reqwest::StatusCode;
use url::Url;
use uuid::Uuid;

use crate::credential::{self, HostCredential};

pub async fn register(
    gateway: &str,
    registration_token: String,
    installation_id: Uuid,
    state_directory: &Path,
    allow_insecure_http: bool,
) -> anyhow::Result<HostCredential> {
    let endpoint = registration_endpoint(gateway)?;
    let response = reqwest::Client::new()
        .post(endpoint)
        .json(&ClaimRegisteredHostRequest {
            registration_token,
            installation_id,
            daemon_version: env!("CARGO_PKG_VERSION").to_owned(),
        })
        .send()
        .await
        .context("claim Registered Host")?;
    let status = response.status();
    if status != StatusCode::OK {
        let message = response
            .text()
            .await
            .unwrap_or_else(|_| "gateway returned an unreadable error".to_owned());
        anyhow::bail!("gateway rejected Registered Host claim ({status}): {message}");
    }
    let claimed: ClaimRegisteredHostResponse = response
        .json()
        .await
        .context("decode Registered Host claim")?;
    let websocket_url = Url::parse(&claimed.websocket_url)
        .context("gateway returned an invalid Host Daemon WebSocket URL")?;
    if websocket_url.scheme() != "wss" && !(allow_insecure_http && websocket_url.scheme() == "ws") {
        anyhow::bail!("gateway returned an insecure Host Daemon WebSocket URL");
    }
    let value = HostCredential {
        host_id: claimed.host_id,
        credential: claimed.credential,
        gateway_url: gateway.trim_end_matches('/').to_owned(),
        websocket_url: claimed.websocket_url,
    };
    credential::save(state_directory, &value)
        .await
        .context("persist Host Daemon credential")?;
    Ok(value)
}

fn registration_endpoint(gateway: &str) -> anyhow::Result<Url> {
    let base = format!("{}/", gateway.trim_end_matches('/'));
    Url::parse(&base)
        .context("invalid Execution Gateway URL")?
        .join("v1/registered-hosts/claim")
        .context("construct Registered Host claim URL")
}
