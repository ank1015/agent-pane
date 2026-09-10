use std::future::Future;

use reqwest::{Client, RequestBuilder, Response, Url};
use serde::{Deserialize, Serialize, de::DeserializeOwned};

use crate::{E2bConfig, E2bError, E2bErrorKind, E2bResult, config::validate_identifier};

/// Supported base-sandbox memory tiers, in MiB.
pub const EXECUTION_BASE_RAM_OPTIONS_MB: [u32; 4] = [1024, 2048, 4096, 8192];
/// Default base-sandbox memory tier, in MiB.
pub const DEFAULT_EXECUTION_BASE_RAM_MB: u32 = 2048;
/// Public E2B template IDs containing the compatible execution-supervisor binary.
pub const EXECUTION_BASE_TEMPLATE_1024_MB_ID: &str = "gbiubfcth0yh0qke09xr";
pub const EXECUTION_BASE_TEMPLATE_2048_MB_ID: &str = "rpffhldnk5j55ptiu283";
pub const EXECUTION_BASE_TEMPLATE_4096_MB_ID: &str = "0eslaqgop81dfuolq66a";
pub const EXECUTION_BASE_TEMPLATE_8192_MB_ID: &str = "11whj4oo5h2x55p3fzj1";
/// Backwards-compatible name for the default base template.
pub const EXECUTION_BASE_TEMPLATE_ID: &str = EXECUTION_BASE_TEMPLATE_2048_MB_ID;

/// Current state reported by the E2B control plane.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum SandboxState {
    Running,
    Paused,
    Other(String),
}

impl<'de> Deserialize<'de> for SandboxState {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        let value = String::deserialize(deserializer)?;
        Ok(match value.as_str() {
            "running" => Self::Running,
            "paused" => Self::Paused,
            _ => Self::Other(value),
        })
    }
}

#[derive(Clone, Deserialize)]
pub struct SandboxDetails {
    #[serde(rename = "sandboxID")]
    pub sandbox_id: String,
    #[serde(rename = "templateID")]
    pub template_id: String,
    pub state: SandboxState,
    #[serde(rename = "envdAccessToken")]
    pub envd_access_token: Option<String>,
    #[serde(default)]
    pub domain: Option<String>,
}

impl std::fmt::Debug for SandboxDetails {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("SandboxDetails")
            .field("sandbox_id", &self.sandbox_id)
            .field("template_id", &self.template_id)
            .field("state", &self.state)
            .field(
                "envd_access_token",
                &self.envd_access_token.as_ref().map(|_| "[REDACTED]"),
            )
            .field("domain", &self.domain)
            .finish()
    }
}

/// Fresh routing and authentication details returned by E2B connect.
#[derive(Clone)]
pub struct ConnectedSandbox {
    pub sandbox_id: String,
    pub domain: Option<String>,
    pub(crate) envd_access_token: String,
}

impl std::fmt::Debug for ConnectedSandbox {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("ConnectedSandbox")
            .field("sandbox_id", &self.sandbox_id)
            .field("domain", &self.domain)
            .field("envd_access_token", &"[REDACTED]")
            .finish()
    }
}

impl ConnectedSandbox {
    pub(crate) fn access_token(&self) -> &str {
        &self.envd_access_token
    }
}

/// E2B control-plane client. It owns lifecycle, not execution semantics.
#[derive(Clone, Debug)]
pub struct E2bControlClient {
    client: Client,
    config: E2bConfig,
}

impl E2bControlClient {
    pub fn new(config: E2bConfig) -> E2bResult<Self> {
        config.validate()?;
        let client = Client::builder()
            .timeout(config.request_timeout)
            .build()
            .map_err(|error| E2bError::configuration(error.to_string()))?;
        Ok(Self { client, config })
    }

    #[must_use]
    pub fn config(&self) -> &E2bConfig {
        &self.config
    }

    /// Verifies that the configured API key is accepted by E2B without
    /// creating, resuming, or otherwise mutating a sandbox.
    pub async fn verify_credentials(&self) -> E2bResult<()> {
        let url = self.endpoint("sandboxes")?;
        retry(&self.config, || async {
            let response = self
                .client
                .get(url.clone())
                .header("X-API-Key", self.config.api_key.expose())
                .query(&[("limit", 1_u8)])
                .send()
                .await
                .map_err(|error| E2bError::from_request(error, true))?;
            if response.status().is_success() {
                Ok(())
            } else {
                let status = response.status();
                let body = response.text().await.unwrap_or_default();
                Err(E2bError::from_response(status, &body, true))
            }
        })
        .await
    }

    /// Creates the standard E2B base environment. Network access defaults to
    /// enabled and RAM defaults to 2048 MiB when omitted.
    ///
    /// Creation is deliberately attempted once. A disconnected POST has an
    /// ambiguous result and replaying it could leak a second billable sandbox.
    pub async fn create_base_sandbox(
        &self,
        network_access: Option<bool>,
        ram_mb: Option<u32>,
    ) -> E2bResult<String> {
        let template_id = execution_base_template_id(ram_mb)?;
        self.create_sandbox_from_template(template_id, network_access)
            .await
    }

    /// Creates an E2B sandbox using a persistent snapshot as its template.
    /// Network access defaults to enabled when `network_access` is `None`.
    pub async fn create_snapshot_sandbox(
        &self,
        snapshot_id: &str,
        network_access: Option<bool>,
    ) -> E2bResult<String> {
        validate_identifier(snapshot_id, "snapshot ID")?;
        self.create_sandbox_from_template(snapshot_id, network_access)
            .await
    }

    /// Creates a persistent snapshot after first connecting to (and therefore
    /// resuming, when necessary) the source sandbox.
    pub async fn snapshot_sandbox(&self, sandbox_id: &str) -> E2bResult<String> {
        validate_identifier(sandbox_id, "sandbox ID")?;
        self.connect_response(sandbox_id).await?;

        let url = self.endpoint(&format!("sandboxes/{sandbox_id}/snapshots"))?;
        let response = self
            .client
            .post(url)
            .header("X-API-Key", self.config.api_key.expose())
            .json(&SnapshotRequest {})
            .send()
            .await
            .map_err(|error| E2bError::from_request(error, false))?;
        checked_json::<SnapshotResponse>(response, false)
            .await
            .map(|value| value.snapshot_id)
    }

    /// Gets status without changing the sandbox lifecycle.
    pub async fn get_sandbox(&self, sandbox_id: &str) -> E2bResult<SandboxDetails> {
        validate_identifier(sandbox_id, "sandbox ID")?;
        let url = self.endpoint(&format!("sandboxes/{sandbox_id}"))?;
        self.send_replayable(|| {
            self.client
                .get(url.clone())
                .header("X-API-Key", self.config.api_key.expose())
        })
        .await
    }

    /// Permanently terminates a sandbox. Missing sandboxes are already in the
    /// desired state, making this operation safe to replay during cleanup.
    pub async fn delete_sandbox(&self, sandbox_id: &str) -> E2bResult<()> {
        validate_identifier(sandbox_id, "sandbox ID")?;
        self.delete_replayable(&format!("sandboxes/{sandbox_id}"))
            .await
    }

    /// Deletes the template backing a snapshot created without a persistent
    /// name. This is primarily useful for lifecycle cleanup.
    pub async fn delete_snapshot(&self, snapshot_id: &str) -> E2bResult<()> {
        validate_identifier(snapshot_id, "snapshot ID")?;
        self.delete_replayable(&format!("templates/{snapshot_id}"))
            .await
    }

    /// Pauses a sandbox while preserving memory. Connect is the corresponding
    /// resume operation. The request is not blindly replayed because a lost
    /// response leaves its outcome ambiguous.
    pub async fn pause_sandbox(&self, sandbox_id: &str) -> E2bResult<()> {
        validate_identifier(sandbox_id, "sandbox ID")?;
        let response = self
            .client
            .post(self.endpoint(&format!("sandboxes/{sandbox_id}/pause"))?)
            .header("X-API-Key", self.config.api_key.expose())
            .json(&PauseRequest { memory: true })
            .send()
            .await
            .map_err(|error| E2bError::from_request(error, false))?;
        if response.status().is_success() {
            Ok(())
        } else {
            let status = response.status();
            let body = response.text().await.unwrap_or_default();
            Err(E2bError::from_response(status, &body, false))
        }
    }

    /// Establishes fresh envd credentials and resumes a paused sandbox.
    ///
    /// E2B defines `POST /connect` as an idempotent lifecycle operation: it
    /// returns details for a running sandbox, resumes a paused sandbox, and only
    /// extends its TTL. It is therefore safe to retry on transient failures.
    pub async fn ensure_connected(&self, sandbox_id: &str) -> E2bResult<ConnectedSandbox> {
        validate_identifier(sandbox_id, "sandbox ID")?;
        let response = self.connect_response(sandbox_id).await?;
        let access_token = response.envd_access_token.ok_or_else(|| {
            E2bError::protocol("E2B connect response did not include envdAccessToken")
        })?;
        Ok(ConnectedSandbox {
            sandbox_id: response.sandbox_id,
            domain: response.domain,
            envd_access_token: access_token,
        })
    }

    async fn create_sandbox_from_template(
        &self,
        template_id: &str,
        network_access: Option<bool>,
    ) -> E2bResult<String> {
        let response = self
            .client
            .post(self.endpoint("sandboxes")?)
            .header("X-API-Key", self.config.api_key.expose())
            .json(&CreateSandboxRequest {
                template_id,
                timeout: self.config.sandbox_timeout_seconds,
                auto_pause: true,
                auto_pause_memory: true,
                auto_resume: AutoResumeConfig { enabled: true },
                secure: true,
                allow_internet_access: network_access.unwrap_or(true),
            })
            .send()
            .await
            .map_err(|error| E2bError::from_request(error, false))?;
        checked_json::<ConnectionResponse>(response, false)
            .await
            .map(|value| value.sandbox_id)
    }

    async fn connect_response(&self, sandbox_id: &str) -> E2bResult<ConnectionResponse> {
        let url = self.endpoint(&format!("sandboxes/{sandbox_id}/connect"))?;
        self.send_replayable(|| {
            self.client
                .post(url.clone())
                .header("X-API-Key", self.config.api_key.expose())
                .json(&ConnectRequest {
                    timeout: self.config.sandbox_timeout_seconds,
                    memory: true,
                })
        })
        .await
    }

    async fn send_replayable<T, F>(&self, build: F) -> E2bResult<T>
    where
        T: DeserializeOwned,
        F: Fn() -> RequestBuilder,
    {
        retry(&self.config, || async {
            let response = build()
                .send()
                .await
                .map_err(|error| E2bError::from_request(error, true))?;
            checked_json(response, true).await
        })
        .await
    }

    async fn delete_replayable(&self, path: &str) -> E2bResult<()> {
        let url = self.endpoint(path)?;
        retry(&self.config, || async {
            let response = self
                .client
                .delete(url.clone())
                .header("X-API-Key", self.config.api_key.expose())
                .send()
                .await
                .map_err(|error| E2bError::from_request(error, true))?;
            if response.status().is_success() || response.status() == reqwest::StatusCode::NOT_FOUND
            {
                Ok(())
            } else {
                let status = response.status();
                let body = response.text().await.unwrap_or_default();
                Err(E2bError::from_response(status, &body, true))
            }
        })
        .await
    }

    fn endpoint(&self, path: &str) -> E2bResult<Url> {
        let mut url = self.config.control_base_url.clone();
        url.path_segments_mut()
            .map_err(|()| E2bError::configuration("E2B control URL cannot be a base URL"))?
            .pop_if_empty()
            .extend(path.split('/'));
        Ok(url)
    }
}

/// Gateway convenience API using `E2B_API_KEY`.
pub async fn create_base_sandbox(
    network_access: Option<bool>,
    ram_mb: Option<u32>,
) -> E2bResult<String> {
    E2bControlClient::new(E2bConfig::from_env()?)?
        .create_base_sandbox(network_access, ram_mb)
        .await
}

/// Resolves a supported RAM tier to its public E2B template ID.
pub fn execution_base_template_id(ram_mb: Option<u32>) -> E2bResult<&'static str> {
    match ram_mb.unwrap_or(DEFAULT_EXECUTION_BASE_RAM_MB) {
        1024 => Ok(EXECUTION_BASE_TEMPLATE_1024_MB_ID),
        2048 => Ok(EXECUTION_BASE_TEMPLATE_2048_MB_ID),
        4096 => Ok(EXECUTION_BASE_TEMPLATE_4096_MB_ID),
        8192 => Ok(EXECUTION_BASE_TEMPLATE_8192_MB_ID),
        value => Err(E2bError::configuration(format!(
            "unsupported base sandbox RAM {value} MiB; expected one of 1024, 2048, 4096, or 8192"
        ))),
    }
}

/// Gateway convenience API using `E2B_API_KEY`.
pub async fn create_snapshot_sandbox(
    e2b_snapshot_id: &str,
    network_access: Option<bool>,
) -> E2bResult<String> {
    E2bControlClient::new(E2bConfig::from_env()?)?
        .create_snapshot_sandbox(e2b_snapshot_id, network_access)
        .await
}

/// Gateway convenience API using `E2B_API_KEY`.
pub async fn snapshot_sandbox(e2b_sandbox_id: &str) -> E2bResult<String> {
    E2bControlClient::new(E2bConfig::from_env()?)?
        .snapshot_sandbox(e2b_sandbox_id)
        .await
}

pub(crate) async fn retry<T, F, Fut>(config: &E2bConfig, mut operation: F) -> E2bResult<T>
where
    F: FnMut() -> Fut,
    Fut: Future<Output = E2bResult<T>>,
{
    let mut attempt = 1;
    loop {
        match operation().await {
            Ok(value) => return Ok(value),
            Err(error) if error.retryable && attempt < config.retry.max_attempts => {
                let delay = config.retry.delay_for_retry(attempt - 1);
                tokio::time::sleep(delay).await;
                attempt += 1;
            }
            Err(error) => return Err(error),
        }
    }
}

async fn checked_json<T: DeserializeOwned>(
    response: Response,
    operation_is_replayable: bool,
) -> E2bResult<T> {
    let status = response.status();
    let body = response
        .bytes()
        .await
        .map_err(|error| E2bError::from_request(error, operation_is_replayable))?;
    if !status.is_success() {
        return Err(E2bError::from_response(
            status,
            &String::from_utf8_lossy(&body),
            operation_is_replayable,
        ));
    }
    serde_json::from_slice(&body).map_err(|error| {
        E2bError::new(
            E2bErrorKind::Protocol,
            format!("E2B returned invalid JSON: {error}"),
        )
    })
}

#[derive(Serialize)]
struct CreateSandboxRequest<'a> {
    #[serde(rename = "templateID")]
    template_id: &'a str,
    timeout: u64,
    #[serde(rename = "autoPause")]
    auto_pause: bool,
    #[serde(rename = "autoPauseMemory")]
    auto_pause_memory: bool,
    #[serde(rename = "autoResume")]
    auto_resume: AutoResumeConfig,
    secure: bool,
    allow_internet_access: bool,
}

#[derive(Serialize)]
struct AutoResumeConfig {
    enabled: bool,
}

#[derive(Serialize)]
struct ConnectRequest {
    timeout: u64,
    memory: bool,
}

#[derive(Serialize)]
struct PauseRequest {
    memory: bool,
}

#[derive(Deserialize)]
struct ConnectionResponse {
    #[serde(rename = "sandboxID")]
    sandbox_id: String,
    #[serde(rename = "envdAccessToken")]
    envd_access_token: Option<String>,
    #[serde(default)]
    domain: Option<String>,
}

#[derive(Serialize)]
struct SnapshotRequest {}

#[derive(Deserialize)]
struct SnapshotResponse {
    #[serde(rename = "snapshotID")]
    snapshot_id: String,
}
