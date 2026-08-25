use std::time::Duration;

use agent_contracts::{
    AcknowledgeRunAbort, AgentApiError, AgentErrorResponse, AppendRunMessages, ClaimRun,
    ClaimedRun, CompleteRunTurn, FailRunTurn, HeartbeatRun, RequestRunWait, RunAbortAcknowledged,
    RunHeartbeat, RunMessagesAppended, RunTurnCompleted, RunTurnFailed, RunWaitRequested,
    SessionMessage, SessionMessagePage,
};
use reqwest::{RequestBuilder, StatusCode, header};
use serde::{Serialize, de::DeserializeOwned};
use url::Url;
use uuid::Uuid;

use crate::config::AgentServiceConfig;

const LEASE_TOKEN_HEADER: &str = "x-agent-lease-token";
const MESSAGE_PAGE_SIZE: u32 = 500;
const DEFAULT_CLAIM_RETRY: Duration = Duration::from_secs(1);

#[derive(Clone)]
pub struct AgentClient {
    http: reqwest::Client,
    base_url: Url,
}

impl AgentClient {
    pub fn new(config: AgentServiceConfig) -> Result<Self, AgentClientError> {
        let mut authorization =
            header::HeaderValue::from_str(&format!("Bearer {}", config.worker_token))
                .map_err(AgentClientError::InvalidWorkerToken)?;
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

    pub async fn claim(
        &self,
        command: &ClaimRun,
        lease_token: &str,
    ) -> Result<ClaimResponse, AgentClientError> {
        let url = endpoint(&self.base_url, &["v1", "worker", "runs", "claim"])?;
        let response = self
            .with_lease(self.http.post(url).json(command), lease_token)?
            .send()
            .await
            .map_err(AgentClientError::Request)?;
        if response.status() == StatusCode::NO_CONTENT {
            let retry_after = response
                .headers()
                .get(header::RETRY_AFTER)
                .and_then(|value| value.to_str().ok())
                .and_then(|value| value.parse::<u64>().ok())
                .map(Duration::from_secs)
                .unwrap_or(DEFAULT_CLAIM_RETRY);
            return Ok(ClaimResponse::Empty { retry_after });
        }
        Ok(ClaimResponse::Claimed(Box::new(read_json(response).await?)))
    }

    pub async fn heartbeat(
        &self,
        run_id: Uuid,
        command: &HeartbeatRun,
        lease_token: &str,
    ) -> Result<RunHeartbeat, AgentClientError> {
        self.post_run(run_id, &["heartbeat"], command, lease_token)
            .await
    }

    /// Fetches the complete committed session history visible to a claimed run.
    pub async fn fetch_session_messages(
        &self,
        run_id: Uuid,
        lease_version: u64,
        lease_token: &str,
    ) -> Result<Vec<SessionMessage>, AgentClientError> {
        let mut messages = Vec::new();
        let mut after_revision: Option<u64> = None;
        loop {
            let run_id = run_id.to_string();
            let mut url = endpoint(
                &self.base_url,
                &["v1", "worker", "runs", &run_id, "messages"],
            )?;
            url.query_pairs_mut()
                .append_pair("lease_version", &lease_version.to_string())
                .append_pair("limit", &MESSAGE_PAGE_SIZE.to_string());
            if let Some(revision) = after_revision {
                url.query_pairs_mut()
                    .append_pair("after_revision", &revision.to_string());
            }
            let response = self
                .with_lease(self.http.get(url), lease_token)?
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
        command: &AppendRunMessages,
        lease_token: &str,
    ) -> Result<RunMessagesAppended, AgentClientError> {
        self.post_run(run_id, &["messages"], command, lease_token)
            .await
    }

    pub async fn complete(
        &self,
        run_id: Uuid,
        command: &CompleteRunTurn,
        lease_token: &str,
    ) -> Result<RunTurnCompleted, AgentClientError> {
        self.post_run(run_id, &["complete"], command, lease_token)
            .await
    }

    pub async fn fail(
        &self,
        run_id: Uuid,
        command: &FailRunTurn,
        lease_token: &str,
    ) -> Result<RunTurnFailed, AgentClientError> {
        self.post_run(run_id, &["fail"], command, lease_token).await
    }

    pub async fn request_wait(
        &self,
        run_id: Uuid,
        command: &RequestRunWait,
        lease_token: &str,
    ) -> Result<RunWaitRequested, AgentClientError> {
        self.post_run(run_id, &["wait"], command, lease_token).await
    }

    pub async fn acknowledge_abort(
        &self,
        run_id: Uuid,
        command: &AcknowledgeRunAbort,
        lease_token: &str,
    ) -> Result<RunAbortAcknowledged, AgentClientError> {
        self.post_run(run_id, &["abort", "acknowledge"], command, lease_token)
            .await
    }

    async fn post_run<T, R>(
        &self,
        run_id: Uuid,
        suffix: &[&str],
        command: &T,
        lease_token: &str,
    ) -> Result<R, AgentClientError>
    where
        T: Serialize + ?Sized,
        R: DeserializeOwned,
    {
        let run_id = run_id.to_string();
        let mut segments = vec!["v1", "worker", "runs", run_id.as_str()];
        segments.extend_from_slice(suffix);
        let url = endpoint(&self.base_url, &segments)?;
        let response = self
            .with_lease(self.http.post(url).json(command), lease_token)?
            .send()
            .await
            .map_err(AgentClientError::Request)?;
        read_json(response).await
    }

    fn with_lease(
        &self,
        request: RequestBuilder,
        lease_token: &str,
    ) -> Result<RequestBuilder, AgentClientError> {
        let mut lease_token = header::HeaderValue::from_str(lease_token)
            .map_err(AgentClientError::InvalidLeaseToken)?;
        lease_token.set_sensitive(true);
        Ok(request.header(LEASE_TOKEN_HEADER, lease_token))
    }
}

#[derive(Clone, Debug, PartialEq)]
pub enum ClaimResponse {
    Claimed(Box<ClaimedRun>),
    Empty { retry_after: Duration },
}

#[derive(Debug, thiserror::Error)]
pub enum AgentClientError {
    #[error("Agent worker token is not a valid HTTP header value")]
    InvalidWorkerToken(#[source] header::InvalidHeaderValue),
    #[error("Agent run lease token is not a valid HTTP header value")]
    InvalidLeaseToken(#[source] header::InvalidHeaderValue),
    #[error("could not build the Agent HTTP client")]
    BuildClient(#[source] reqwest::Error),
    #[error("could not construct the Agent worker URL")]
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
    pub fn lease_is_lost(&self) -> bool {
        matches!(
            self.code(),
            Some("run_lease_lost" | "invalid_run_lease" | "run_not_runnable")
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
