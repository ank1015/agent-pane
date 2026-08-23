use std::path::Path;

use anyhow::Context;
use execution_contracts::EnvironmentDescriptor;
use execution_protocol::{ClaimMachineRequest, ClaimMachineResponse};
use reqwest::{StatusCode, Url};

use crate::credential::{self, CloudCredential};

pub async fn register(
    gateway: &str,
    registration_token: String,
    descriptor: &EnvironmentDescriptor,
    state_directory: &Path,
) -> anyhow::Result<CloudCredential> {
    let endpoint = registration_endpoint(gateway)?;
    let response = reqwest::Client::new()
        .post(endpoint)
        .json(&ClaimMachineRequest {
            registration_token,
            descriptor: descriptor.clone(),
        })
        .send()
        .await
        .context("claim machine registration")?;
    let status = response.status();
    if status != StatusCode::OK {
        let message = response
            .text()
            .await
            .unwrap_or_else(|_| "gateway returned an unreadable error".to_owned());
        anyhow::bail!("gateway rejected machine registration ({status}): {message}");
    }
    let claimed: ClaimMachineResponse = response
        .json()
        .await
        .context("decode machine registration response")?;
    if claimed.machine_id != descriptor.machine_id {
        anyhow::bail!("gateway returned a credential for a different machine");
    }
    let value = CloudCredential {
        machine_id: claimed.machine_id,
        credential: claimed.credential,
        gateway_url: gateway.trim_end_matches('/').to_owned(),
        websocket_url: claimed.websocket_url,
    };
    credential::save(state_directory, &value)
        .await
        .context("persist machine credential")?;
    Ok(value)
}

fn registration_endpoint(gateway: &str) -> anyhow::Result<Url> {
    let base = format!("{}/", gateway.trim_end_matches('/'));
    Url::parse(&base)
        .context("invalid execution gateway URL")?
        .join("v1/machine-registrations/claim")
        .context("construct machine registration endpoint")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn appends_claim_path_to_gateway_root() {
        assert_eq!(
            registration_endpoint("https://gateway.example.com/base")
                .expect("endpoint")
                .as_str(),
            "https://gateway.example.com/base/v1/machine-registrations/claim"
        );
    }
}
