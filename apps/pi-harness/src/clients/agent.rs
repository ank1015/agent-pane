use agent_contracts::{
    AgentApiError, AgentErrorResponse, AppendSessionMessages, NewRunMessage, SessionMessage,
    SessionMessagePage, SessionMessagesAppended,
};
use reqwest::{StatusCode, header};
use serde::de::DeserializeOwned;
use url::Url;
use uuid::Uuid;

use crate::config::AgentServiceConfig;

const MESSAGE_PAGE_SIZE: u32 = 500;

#[derive(Clone)]
pub struct AgentClient {
    http: reqwest::Client,
    base_url: Url,
}

impl AgentClient {
    pub fn new(config: AgentServiceConfig) -> Result<Self, AgentClientError> {
        let mut authorization =
            header::HeaderValue::from_str(&format!("Bearer {}", config.harness_token))
                .map_err(AgentClientError::InvalidHarnessToken)?;
        authorization.set_sensitive(true);
        let mut headers = header::HeaderMap::new();
        headers.insert(header::AUTHORIZATION, authorization);
        headers.insert(
            header::ACCEPT,
            header::HeaderValue::from_static("application/json"),
        );
        let http = reqwest::Client::builder()
            .default_headers(headers)
            .timeout(config.request_timeout)
            .build()
            .map_err(AgentClientError::BuildClient)?;
        Ok(Self {
            http,
            base_url: config.base_url,
        })
    }

    pub async fn fetch_session_messages(
        &self,
        run_id: Uuid,
    ) -> Result<Vec<SessionMessage>, AgentClientError> {
        let mut messages = Vec::new();
        let mut after_revision: Option<u64> = None;
        loop {
            let run_id = run_id.to_string();
            let mut url = endpoint(
                &self.base_url,
                &["v1", "harness", "runs", &run_id, "messages"],
            )?;
            url.query_pairs_mut()
                .append_pair("limit", &MESSAGE_PAGE_SIZE.to_string());
            if let Some(revision) = after_revision {
                url.query_pairs_mut()
                    .append_pair("after_revision", &revision.to_string());
            }
            let response = self
                .http
                .get(url)
                .send()
                .await
                .map_err(AgentClientError::Request)?;
            let page = read_json::<SessionMessagePage>(response).await?;
            messages.extend(page.items);
            match page.next_after_revision {
                Some(revision) => after_revision = Some(revision),
                None => return Ok(messages),
            }
        }
    }

    pub async fn append_messages(
        &self,
        run_id: Uuid,
        expected_state_version: u64,
        turn_number: u32,
        expected_session_revision: u64,
        messages: Vec<NewRunMessage>,
    ) -> Result<SessionMessagesAppended, AgentClientError> {
        let run_id = run_id.to_string();
        let url = endpoint(
            &self.base_url,
            &["v1", "harness", "runs", &run_id, "messages"],
        )?;
        let response = self
            .http
            .post(url)
            .json(&AppendSessionMessages {
                expected_state_version,
                turn_number,
                expected_session_revision,
                messages,
            })
            .send()
            .await
            .map_err(AgentClientError::Request)?;
        read_json(response).await
    }
}

#[derive(Debug, thiserror::Error)]
pub enum AgentClientError {
    #[error("Agent harness token is not a valid HTTP header value")]
    InvalidHarnessToken(#[source] header::InvalidHeaderValue),
    #[error("could not build the Agent HTTP client")]
    BuildClient(#[source] reqwest::Error),
    #[error("could not construct the Agent harness URL")]
    InvalidUrl,
    #[error("Agent request failed")]
    Request(#[source] reqwest::Error),
    #[error("Agent rejected the request with status {status}")]
    Rejected {
        status: StatusCode,
        error: Option<AgentApiError>,
        body: String,
    },
    #[error("Agent returned invalid JSON")]
    InvalidResponse(#[source] serde_json::Error),
}

impl AgentClientError {
    #[must_use]
    pub fn code(&self) -> Option<&str> {
        match self {
            Self::Rejected {
                error: Some(error), ..
            } => Some(&error.code),
            _ => None,
        }
    }

    #[must_use]
    pub fn stale(&self) -> bool {
        matches!(
            self.code(),
            Some("run_not_found" | "run_state_conflict" | "run_turn_conflict" | "run_not_active")
        )
    }

    #[must_use]
    pub fn retryable(&self) -> bool {
        match self {
            Self::Request(_) => true,
            Self::Rejected { status, .. } => {
                status.is_server_error() || *status == StatusCode::TOO_MANY_REQUESTS
            }
            _ => false,
        }
    }
}

async fn read_json<T: DeserializeOwned>(
    response: reqwest::Response,
) -> Result<T, AgentClientError> {
    let status = response.status();
    let body = response.bytes().await.map_err(AgentClientError::Request)?;
    if !status.is_success() {
        let error = serde_json::from_slice::<AgentErrorResponse>(&body)
            .ok()
            .map(|response| response.error);
        return Err(AgentClientError::Rejected {
            status,
            error,
            body: String::from_utf8_lossy(&body).into_owned(),
        });
    }
    serde_json::from_slice(&body).map_err(AgentClientError::InvalidResponse)
}

fn endpoint(base_url: &Url, segments: &[&str]) -> Result<Url, AgentClientError> {
    let mut url = base_url.clone();
    let mut path = url
        .path_segments_mut()
        .map_err(|()| AgentClientError::InvalidUrl)?;
    path.pop_if_empty();
    path.extend(segments.iter().copied());
    drop(path);
    Ok(url)
}
