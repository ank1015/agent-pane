use std::time::Duration;

use axum::http::StatusCode;
use reqwest::header;
use serde::de::DeserializeOwned;
use url::Url;

use crate::{error::agent_rejected_body, harnesses::model::AgentHarnessPage};

const HARNESS_PAGE_SIZE: u32 = 100;

#[derive(Clone)]
pub struct AgentClient {
    http: reqwest::Client,
    base_url: Url,
}

impl AgentClient {
    pub fn new(
        base_url: Url,
        control_token: &str,
        timeout: Duration,
    ) -> Result<Self, AgentClientError> {
        let mut authorization =
            reqwest::header::HeaderValue::from_str(&format!("Bearer {control_token}"))
                .map_err(AgentClientError::InvalidControlToken)?;
        authorization.set_sensitive(true);
        let mut headers = reqwest::header::HeaderMap::new();
        headers.insert(header::AUTHORIZATION, authorization);

        let http = reqwest::Client::builder()
            .default_headers(headers)
            .timeout(timeout)
            .build()
            .map_err(AgentClientError::Build)?;

        Ok(Self {
            http,
            base_url: normalized_base_url(base_url),
        })
    }

    pub(crate) async fn ready(&self) -> Result<(), AgentError> {
        self.send_empty(self.http.get(self.url("ready")?)).await
    }

    pub(crate) async fn list_harnesses(
        &self,
        cursor: Option<&str>,
    ) -> Result<AgentHarnessPage, AgentError> {
        let mut request = self
            .http
            .get(self.url("v1/harnesses")?)
            .query(&[("limit", HARNESS_PAGE_SIZE)]);
        if let Some(cursor) = cursor {
            request = request.query(&[("cursor", cursor)]);
        }
        self.send_json(request).await
    }

    fn url(&self, path: &str) -> Result<Url, AgentError> {
        self.base_url.join(path).map_err(AgentError::InvalidUrl)
    }

    async fn send_json<T: DeserializeOwned>(
        &self,
        request: reqwest::RequestBuilder,
    ) -> Result<T, AgentError> {
        let response = request.send().await.map_err(AgentError::Request)?;
        let status = response.status();
        let body = response.bytes().await.map_err(AgentError::Request)?;
        if !status.is_success() {
            return Err(AgentError::Rejected {
                status,
                body: agent_rejected_body(&body),
            });
        }
        serde_json::from_slice(&body).map_err(AgentError::InvalidResponse)
    }

    async fn send_empty(&self, request: reqwest::RequestBuilder) -> Result<(), AgentError> {
        let response = request.send().await.map_err(AgentError::Request)?;
        let status = response.status();
        if status.is_success() {
            return Ok(());
        }
        let body = response.bytes().await.map_err(AgentError::Request)?;
        Err(AgentError::Rejected {
            status,
            body: agent_rejected_body(&body),
        })
    }
}

fn normalized_base_url(mut base_url: Url) -> Url {
    if !base_url.path().ends_with('/') {
        let path = format!("{}/", base_url.path());
        base_url.set_path(&path);
    }
    base_url
}

#[derive(Debug, thiserror::Error)]
pub enum AgentClientError {
    #[error("PLATFORM_AGENT_CONTROL_TOKEN is not a valid HTTP header value")]
    InvalidControlToken(#[source] reqwest::header::InvalidHeaderValue),
    #[error("could not build the Agent HTTP client")]
    Build(#[source] reqwest::Error),
}

#[derive(Debug, thiserror::Error)]
pub enum AgentError {
    #[error("could not construct an Agent URL")]
    InvalidUrl(#[source] url::ParseError),
    #[error("Agent request failed")]
    Request(#[source] reqwest::Error),
    #[error("Agent returned an invalid response")]
    InvalidResponse(#[source] serde_json::Error),
    #[error("Agent returned a repeated pagination cursor")]
    InvalidPagination,
    #[error("Agent rejected the request with status {status}")]
    Rejected {
        status: StatusCode,
        body: serde_json::Value,
    },
}

#[cfg(test)]
#[path = "agent_tests.rs"]
mod tests;
