use agent_contracts::{AgentApiError, AgentErrorResponse};
use llm_contracts::JsonObject;
use reqwest::{Method, StatusCode, header};
use serde::Serialize;
use serde_json::json;
use url::Url;

use crate::config::AgentControlServiceConfig;

pub const PI_HARNESS_ID: &str = "pi";
pub const PI_HARNESS_REVISION_ID: &str = "pi-2026-08-26-machine-target";

#[derive(Clone)]
pub struct HarnessRegistryClient {
    http: reqwest::Client,
    base_url: Url,
}

impl HarnessRegistryClient {
    pub fn new(config: AgentControlServiceConfig) -> Result<Self, HarnessRegistryError> {
        let mut authorization =
            header::HeaderValue::from_str(&format!("Bearer {}", config.control_token))
                .map_err(HarnessRegistryError::InvalidControlToken)?;
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
            .map_err(HarnessRegistryError::BuildClient)?;
        Ok(Self {
            http,
            base_url: config.base_url,
        })
    }

    pub async fn create_harness(
        &self,
        command: &CreateHarnessRequest,
    ) -> Result<(), HarnessRegistryError> {
        self.send(Method::POST, &["v1", "harnesses"], command).await
    }

    pub async fn register_revision(
        &self,
        harness_id: &str,
        command: &RegisterHarnessRevisionRequest,
    ) -> Result<(), HarnessRegistryError> {
        self.send(
            Method::POST,
            &["v1", "harnesses", harness_id, "revisions"],
            command,
        )
        .await
    }

    pub async fn activate_revision(
        &self,
        harness_id: &str,
        harness_revision_id: &str,
    ) -> Result<(), HarnessRegistryError> {
        self.send(
            Method::PUT,
            &["v1", "harnesses", harness_id, "active-revision"],
            &SetActiveRevisionRequest {
                harness_revision_id,
            },
        )
        .await
    }

    pub async fn set_enabled(
        &self,
        harness_id: &str,
        enabled: bool,
    ) -> Result<(), HarnessRegistryError> {
        self.send(
            Method::PUT,
            &["v1", "harnesses", harness_id, "enabled"],
            &SetHarnessEnabledRequest { enabled },
        )
        .await
    }

    /// Idempotently provisions the harness revision implemented by this binary.
    pub async fn ensure_pi_harness(&self) -> Result<(), HarnessRegistryError> {
        self.create_harness(&pi_harness()).await?;
        self.register_revision(PI_HARNESS_ID, &pi_revision())
            .await?;
        self.activate_revision(PI_HARNESS_ID, PI_HARNESS_REVISION_ID)
            .await?;
        self.set_enabled(PI_HARNESS_ID, true).await
    }

    async fn send<T: Serialize + ?Sized>(
        &self,
        method: Method,
        segments: &[&str],
        command: &T,
    ) -> Result<(), HarnessRegistryError> {
        let url = endpoint(&self.base_url, segments)?;
        let response = self
            .http
            .request(method, url)
            .json(command)
            .send()
            .await
            .map_err(HarnessRegistryError::Request)?;
        let status = response.status();
        let body = response
            .bytes()
            .await
            .map_err(HarnessRegistryError::Request)?;
        if status.is_success() {
            return Ok(());
        }
        let error = serde_json::from_slice::<AgentErrorResponse>(&body)
            .ok()
            .map(|response| response.error);
        Err(HarnessRegistryError::Rejected { status, error })
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct CreateHarnessRequest {
    pub harness_id: String,
    pub slug: String,
    pub display_name: String,
    pub description: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct RegisterHarnessRevisionRequest {
    pub harness_revision_id: String,
    pub revision: String,
    pub contract_version: u32,
    pub default_config: JsonObject,
    pub config_schema: Option<JsonObject>,
}

#[derive(Serialize)]
struct SetActiveRevisionRequest<'a> {
    harness_revision_id: &'a str,
}

#[derive(Serialize)]
struct SetHarnessEnabledRequest {
    enabled: bool,
}

fn pi_harness() -> CreateHarnessRequest {
    CreateHarnessRequest {
        harness_id: PI_HARNESS_ID.to_owned(),
        slug: "pi".to_owned(),
        display_name: "Pi Coding Agent".to_owned(),
        description: Some("Pi's default four-tool coding harness".to_owned()),
    }
}

fn pi_revision() -> RegisterHarnessRevisionRequest {
    RegisterHarnessRevisionRequest {
        harness_revision_id: PI_HARNESS_REVISION_ID.to_owned(),
        revision: "2026-08-26-machine-target".to_owned(),
        contract_version: 1,
        default_config: object(json!({
            "provider": "openai",
            "model_id": "gpt-5.6-sol",
            "reasoning_level": "high",
            "is_replaced": false
        })),
        config_schema: Some(object(json!({
            "$schema": "https://json-schema.org/draft/2020-12/schema",
            "type": "object",
            "properties": {
                "provider": {"const": "openai"},
                "model_id": {
                    "enum": ["gpt-5.6-sol", "gpt-5.6-terra", "gpt-5.6-luna"]
                },
                "reasoning_level": {
                    "enum": ["low", "medium", "high", "xhigh", "max"]
                },
                "execution": {
                    "type": "object",
                    "properties": {
                        "machine_id": {"type": "string", "minLength": 1},
                        "workspace_root_id": {"type": "string", "minLength": 1},
                        "cwd": {"type": "string", "minLength": 1}
                    },
                    "required": ["machine_id", "workspace_root_id", "cwd"],
                    "additionalProperties": false
                },
                "account_id": {"type": "string", "minLength": 1},
                "external_prompt": {"type": ["string", "null"]},
                "is_replaced": {"type": "boolean"}
            },
            "required": ["provider", "model_id", "reasoning_level", "is_replaced"],
            "additionalProperties": false
        }))),
    }
}

fn object(value: serde_json::Value) -> JsonObject {
    value
        .as_object()
        .expect("static value is an object")
        .clone()
}

fn endpoint(base_url: &Url, segments: &[&str]) -> Result<Url, HarnessRegistryError> {
    let mut url = base_url.clone();
    let mut path = url
        .path_segments_mut()
        .map_err(|()| HarnessRegistryError::InvalidUrl)?;
    path.pop_if_empty();
    path.extend(segments.iter().copied());
    drop(path);
    Ok(url)
}

#[derive(Debug, thiserror::Error)]
pub enum HarnessRegistryError {
    #[error("Agent control token is not a valid HTTP header value")]
    InvalidControlToken(#[source] header::InvalidHeaderValue),
    #[error("could not build the Agent harness registry client")]
    BuildClient(#[source] reqwest::Error),
    #[error("could not construct an Agent harness registry URL")]
    InvalidUrl,
    #[error("Agent harness registry request failed")]
    Request(#[source] reqwest::Error),
    #[error("Agent rejected harness registration with status {status}")]
    Rejected {
        status: StatusCode,
        error: Option<AgentApiError>,
    },
}
