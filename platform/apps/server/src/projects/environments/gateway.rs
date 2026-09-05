use super::{
    error::EnvironmentError,
    model::{CreateEnvironment, EnvironmentType},
};
use chrono::{DateTime, Utc};
use reqwest::{
    Client,
    header::{AUTHORIZATION, HeaderMap, HeaderValue},
};
use serde::Deserialize;
use std::time::Duration;
use url::Url;
use uuid::Uuid;

#[derive(Clone)]
pub struct EnvironmentGateway {
    http: Client,
    base: Url,
}

impl EnvironmentGateway {
    pub fn new(
        mut base: Url,
        token: &str,
        timeout: Duration,
    ) -> Result<Self, Box<dyn std::error::Error>> {
        if !(base.scheme() == "https"
            || (base.scheme() == "http"
                && matches!(base.host_str(), Some("localhost" | "127.0.0.1" | "[::1]"))))
            || !base.username().is_empty()
            || base.password().is_some()
            || base.query().is_some()
            || base.fragment().is_some()
            || timeout.is_zero()
        {
            return Err("Invalid execution gateway configuration for environments".into());
        }
        if !base.path().ends_with('/') {
            base.set_path(&format!("{}/", base.path()));
        }
        let mut headers = HeaderMap::new();
        let mut auth = HeaderValue::from_str(&format!("Bearer {token}"))?;
        auth.set_sensitive(true);
        headers.insert(AUTHORIZATION, auth);
        let http = Client::builder()
            .default_headers(headers)
            .redirect(reqwest::redirect::Policy::none())
            .timeout(timeout)
            .build()?;
        Ok(Self { http, base })
    }

    pub(super) async fn validate(&self, input: &CreateEnvironment) -> Result<(), EnvironmentError> {
        match input.kind {
            EnvironmentType::Machine => {
                let id = input
                    .machine_id
                    .ok_or(EnvironmentError::Invalid("machine_id is required."))?;
                let host: Host = self.get(&format!("v1/hosts/{id}")).await?;
                if host.id != id {
                    return Err(EnvironmentError::Gateway);
                }
                if host.kind != "registered"
                    || host.deleted_at.is_some()
                    || host.desired_state == "deleted"
                {
                    return Err(EnvironmentError::Reference(
                        "Choose a non-deleted registered machine.",
                    ));
                }
                // Offline hosts retain their last descriptor; do not require a live connection.
                let descriptor = host.descriptor.ok_or(EnvironmentError::Reference("This machine has no known roots yet. Connect it once before adding an environment."))?;
                let root = descriptor
                    .roots
                    .into_iter()
                    .find(|root| root.native_path == input.workspace_root)
                    .ok_or(EnvironmentError::Reference(
                        "workspace_root is not a configured root on this machine.",
                    ))?;
                if root.native_path.is_empty()
                    || root.native_path.len() > 4096
                    || root.native_path.chars().any(char::is_control)
                {
                    return Err(EnvironmentError::Gateway);
                }
                Ok(())
            }
            EnvironmentType::Sandbox => {
                let id = input
                    .snapshot_id
                    .ok_or(EnvironmentError::Invalid("snapshot_id is required."))?;
                let snapshot: Snapshot = self.get(&format!("v1/snapshots/{id}")).await?;
                if snapshot.id != id {
                    return Err(EnvironmentError::Gateway);
                }
                if snapshot.state != "ready"
                    || snapshot.deleted_at.is_some()
                    || snapshot.desired_state == "deleted"
                {
                    return Err(EnvironmentError::Reference(
                        "Choose a ready, non-deleted snapshot.",
                    ));
                }
                // Persist the caller's absolute path; verify it against the restored
                // host when used. Do not start a sandbox just to register a record.
                Ok(())
            }
        }
    }

    async fn get<T: serde::de::DeserializeOwned>(&self, path: &str) -> Result<T, EnvironmentError> {
        let mut response = self
            .http
            .get(
                self.base
                    .join(path)
                    .map_err(|_| EnvironmentError::Gateway)?,
            )
            .header("accept", "application/json")
            .send()
            .await
            .map_err(request_error)?;
        if response.status() == reqwest::StatusCode::NOT_FOUND {
            return Err(EnvironmentError::Reference(
                "The referenced machine or snapshot was not found.",
            ));
        }
        if !response.status().is_success() {
            return Err(EnvironmentError::Gateway);
        }
        let mut body = Vec::new();
        while let Some(chunk) = response.chunk().await.map_err(request_error)? {
            if body.len() + chunk.len() > 1024 * 1024 {
                return Err(EnvironmentError::Gateway);
            }
            body.extend_from_slice(&chunk);
        }
        serde_json::from_slice(&body).map_err(|_| EnvironmentError::Gateway)
    }
}

fn request_error(error: reqwest::Error) -> EnvironmentError {
    if error.is_timeout() {
        EnvironmentError::Timeout
    } else {
        EnvironmentError::Gateway
    }
}

// Only deserialize the metadata necessary for reference validation.
#[derive(Deserialize)]
struct Host {
    id: Uuid,
    kind: String,
    desired_state: String,
    deleted_at: Option<DateTime<Utc>>,
    descriptor: Option<Descriptor>,
}
#[derive(Deserialize)]
struct Descriptor {
    roots: Vec<Root>,
}
#[derive(Deserialize)]
struct Root {
    native_path: String,
}
#[derive(Deserialize)]
struct Snapshot {
    id: Uuid,
    state: String,
    desired_state: String,
    deleted_at: Option<DateTime<Utc>>,
}
