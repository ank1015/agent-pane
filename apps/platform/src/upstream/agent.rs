use std::time::Duration;

use agent_contracts::{
    HarnessRevision, NewRunMessage, Run, RunAbort, RunEventPage, RunStatus, SessionMessage,
    SessionMessagePage,
};
use axum::http::StatusCode;
use chrono::{DateTime, Utc};
use llm_contracts::JsonObject;
use reqwest::header;
use serde::{Deserialize, Serialize, de::DeserializeOwned};
use url::Url;
use uuid::Uuid;

use crate::{
    error::agent_rejected_body,
    harnesses::model::{AgentHarness, AgentHarnessPage},
};

const HARNESS_PAGE_SIZE: u32 = 100;

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct AgentSession {
    pub session_id: Uuid,
    pub current_revision: u64,
    pub created_at: DateTime<Utc>,
}

#[derive(Clone, Debug, Serialize)]
struct CreateAgentSession {
    session_id: Uuid,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
#[serde(tag = "selection", rename_all = "snake_case")]
pub enum AgentHarnessSelection {
    ActiveRevision { harness_id: String },
    ExactRevision { harness_revision_id: String },
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, Serialize)]
pub struct AgentRunLimits {
    pub max_turns: Option<u32>,
}

#[derive(Clone, Debug, Serialize)]
pub struct AgentStartRun {
    pub run_id: Uuid,
    pub input: NewRunMessage,
    pub harness: AgentHarnessSelection,
    pub config_override: JsonObject,
    pub limits: AgentRunLimits,
    pub expected_session_revision: Option<u64>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct AgentRunAccepted {
    pub trigger_message: SessionMessage,
    pub run: Run,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct AgentRunAbortRequest {
    pub abort_id: Uuid,
    pub expected_state_version: u64,
    pub reason: Option<String>,
    #[serde(default)]
    pub payload: JsonObject,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct AgentRunAbortResult {
    pub abort: RunAbort,
    pub run: Run,
}

#[derive(Clone, Copy, Debug, Default, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct AgentRunEventStreamQuery {
    pub after_sequence: Option<u64>,
}

#[derive(Clone, Copy, Debug, Default, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct AgentRunEventListQuery {
    pub after_sequence: Option<u64>,
    pub limit: Option<u32>,
}

#[derive(Clone, Copy, Debug, Default, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct AgentSessionMessageListQuery {
    pub after_revision: Option<u64>,
    pub limit: Option<u32>,
    pub run_id: Option<Uuid>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct AgentSessionRunSummary {
    pub run_id: Uuid,
    pub session_id: Uuid,
    pub trigger_message_id: Uuid,
    pub harness_revision_id: String,
    pub status: RunStatus,
    pub current_turn: u32,
    pub max_turns: u32,
    pub state_version: u64,
    pub final_message_id: Option<Uuid>,
    pub failure: Option<serde_json::Value>,
    pub created_at: DateTime<Utc>,
    pub activated_at: DateTime<Utc>,
    pub finished_at: Option<DateTime<Utc>>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct AgentSessionRunPage {
    pub items: Vec<AgentSessionRunSummary>,
    pub next_cursor: Option<String>,
}

#[derive(Clone, Debug, Default, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct AgentSessionRunListQuery {
    pub status: Option<RunStatus>,
    pub limit: Option<u32>,
    pub cursor: Option<String>,
}

#[derive(Clone)]
pub struct AgentClient {
    http: reqwest::Client,
    streaming_http: reqwest::Client,
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
            .default_headers(headers.clone())
            .timeout(timeout)
            .build()
            .map_err(AgentClientError::Build)?;
        let streaming_http = reqwest::Client::builder()
            .default_headers(headers)
            .connect_timeout(timeout)
            .build()
            .map_err(AgentClientError::Build)?;

        Ok(Self {
            http,
            streaming_http,
            base_url: normalized_base_url(base_url),
        })
    }

    pub(crate) async fn ready(&self) -> Result<(), AgentError> {
        self.send_empty(self.http.get(self.url("ready")?)).await
    }

    pub(crate) async fn list_harnesses(
        &self,
        cursor: Option<&str>,
        enabled: Option<bool>,
    ) -> Result<AgentHarnessPage, AgentError> {
        let mut request = self
            .http
            .get(self.url("v1/harnesses")?)
            .query(&[("limit", HARNESS_PAGE_SIZE)]);
        if let Some(cursor) = cursor {
            request = request.query(&[("cursor", cursor)]);
        }
        if let Some(enabled) = enabled {
            request = request.query(&[("enabled", enabled)]);
        }
        self.send_json(request).await
    }

    pub(crate) async fn get_harness(&self, harness_id: &str) -> Result<AgentHarness, AgentError> {
        self.send_json(
            self.http
                .get(self.url_segments(&["v1", "harnesses", harness_id])?),
        )
        .await
    }

    pub(crate) async fn get_harness_revision(
        &self,
        harness_id: &str,
        revision_id: &str,
    ) -> Result<HarnessRevision, AgentError> {
        self.send_json(self.http.get(self.url_segments(&[
            "v1",
            "harnesses",
            harness_id,
            "revisions",
            revision_id,
        ])?))
        .await
    }

    pub(crate) async fn create_session(
        &self,
        session_id: Uuid,
    ) -> Result<AgentSession, AgentError> {
        self.send_json(
            self.http
                .post(self.url_segments(&["v1", "sessions"])?)
                .json(&CreateAgentSession { session_id }),
        )
        .await
    }

    pub(crate) async fn get_session(&self, session_id: Uuid) -> Result<AgentSession, AgentError> {
        self.send_json(self.http.get(self.url_segments(&[
            "v1",
            "sessions",
            &session_id.to_string(),
        ])?))
        .await
    }

    pub(crate) async fn list_session_messages(
        &self,
        session_id: Uuid,
        query: &AgentSessionMessageListQuery,
    ) -> Result<SessionMessagePage, AgentError> {
        self.send_json(
            self.http
                .get(self.url_segments(&["v1", "sessions", &session_id.to_string(), "messages"])?)
                .query(query),
        )
        .await
    }

    pub(crate) async fn list_session_runs(
        &self,
        session_id: Uuid,
        query: &AgentSessionRunListQuery,
    ) -> Result<AgentSessionRunPage, AgentError> {
        self.send_json(
            self.http
                .get(self.url_segments(&["v1", "sessions", &session_id.to_string(), "runs"])?)
                .query(query),
        )
        .await
    }

    pub(crate) async fn start_run(
        &self,
        session_id: Uuid,
        request: &AgentStartRun,
    ) -> Result<AgentRunAccepted, AgentError> {
        self.send_json(
            self.http
                .post(self.url_segments(&["v1", "sessions", &session_id.to_string(), "runs"])?)
                .json(request),
        )
        .await
    }

    pub(crate) async fn get_run(&self, run_id: Uuid) -> Result<Run, AgentError> {
        self.send_json(
            self.http
                .get(self.url_segments(&["v1", "runs", &run_id.to_string()])?),
        )
        .await
    }

    pub(crate) async fn list_run_events(
        &self,
        run_id: Uuid,
        query: &AgentRunEventListQuery,
    ) -> Result<RunEventPage, AgentError> {
        self.send_json(
            self.http
                .get(self.url_segments(&["v1", "runs", &run_id.to_string(), "events"])?)
                .query(query),
        )
        .await
    }

    pub(crate) async fn stream_run_events(
        &self,
        run_id: Uuid,
        query: &AgentRunEventStreamQuery,
        last_event_id: Option<&str>,
    ) -> Result<reqwest::Response, AgentError> {
        let mut request = self
            .streaming_http
            .get(self.url_segments(&["v1", "runs", &run_id.to_string(), "events", "stream"])?)
            .query(query);
        if let Some(last_event_id) = last_event_id {
            request = request.header("last-event-id", last_event_id);
        }
        self.send_stream(request).await
    }

    pub(crate) async fn abort_run(
        &self,
        run_id: Uuid,
        request: &AgentRunAbortRequest,
    ) -> Result<AgentRunAbortResult, AgentError> {
        self.send_json(
            self.http
                .post(self.url_segments(&["v1", "runs", &run_id.to_string(), "abort"])?)
                .json(request),
        )
        .await
    }

    fn url(&self, path: &str) -> Result<Url, AgentError> {
        self.base_url.join(path).map_err(AgentError::InvalidUrl)
    }

    fn url_segments(&self, segments: &[&str]) -> Result<Url, AgentError> {
        let mut url = self.base_url.clone();
        let mut path = url
            .path_segments_mut()
            .map_err(|()| AgentError::InvalidBaseUrl)?;
        path.pop_if_empty();
        path.extend(segments.iter().copied());
        drop(path);
        Ok(url)
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

    async fn send_stream(
        &self,
        request: reqwest::RequestBuilder,
    ) -> Result<reqwest::Response, AgentError> {
        let response = request.send().await.map_err(AgentError::Request)?;
        let status = response.status();
        if status.is_success() {
            return Ok(response);
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
    #[error("Agent base URL cannot be used for path segments")]
    InvalidBaseUrl,
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
