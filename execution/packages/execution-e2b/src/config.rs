use std::{fmt, path::PathBuf, time::Duration};

use execution_core::{ExecutionHostId, RootId};
use url::Url;

use crate::{E2bError, E2bResult};

const DEFAULT_CONTROL_URL: &str = "https://api.e2b.app/";
const DEFAULT_ENVD_DOMAIN: &str = "e2b.app";

/// E2B API credential with a redacted `Debug` implementation.
#[derive(Clone, Eq, PartialEq)]
pub struct E2bApiKey(String);

impl E2bApiKey {
    pub fn new(value: impl Into<String>) -> E2bResult<Self> {
        let value = value.into();
        if value.is_empty() || value.trim() != value {
            return Err(E2bError::configuration(
                "E2B API key must not be blank or contain surrounding whitespace",
            ));
        }
        Ok(Self(value))
    }

    pub fn from_env() -> E2bResult<Self> {
        let value = std::env::var("E2B_API_KEY")
            .map_err(|_| E2bError::configuration("E2B_API_KEY must be set to use execution-e2b"))?;
        Self::new(value)
    }

    pub(crate) fn expose(&self) -> &str {
        &self.0
    }
}

impl fmt::Debug for E2bApiKey {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("E2bApiKey([REDACTED])")
    }
}

/// Bounded exponential-backoff policy for replay-safe E2B calls.
#[derive(Clone, Debug)]
pub struct RetryPolicy {
    pub max_attempts: u32,
    pub initial_delay: Duration,
    pub max_delay: Duration,
}

impl RetryPolicy {
    pub(crate) fn validate(&self) -> E2bResult<()> {
        if self.max_attempts == 0 {
            return Err(E2bError::configuration(
                "retry max_attempts must be greater than zero",
            ));
        }
        if self.initial_delay.is_zero() || self.max_delay.is_zero() {
            return Err(E2bError::configuration(
                "retry delays must be greater than zero",
            ));
        }
        if self.initial_delay > self.max_delay {
            return Err(E2bError::configuration(
                "retry initial_delay must not exceed max_delay",
            ));
        }
        Ok(())
    }

    pub(crate) fn delay_for_retry(&self, retry_index: u32) -> Duration {
        let multiplier = 1u32.checked_shl(retry_index.min(30)).unwrap_or(u32::MAX);
        self.initial_delay
            .saturating_mul(multiplier)
            .min(self.max_delay)
    }
}

impl Default for RetryPolicy {
    fn default() -> Self {
        Self {
            max_attempts: 4,
            initial_delay: Duration::from_millis(100),
            max_delay: Duration::from_secs(2),
        }
    }
}

/// Shared E2B control-plane and envd connection settings.
#[derive(Clone, Debug)]
pub struct E2bConfig {
    pub api_key: E2bApiKey,
    pub control_base_url: Url,
    /// Override used by tests or self-hosted routing. Production derives
    /// `https://sandbox.{domain}` from the control-plane response.
    pub envd_base_url_override: Option<Url>,
    pub envd_port: u16,
    pub username: String,
    pub sandbox_timeout_seconds: u64,
    pub request_timeout: Duration,
    pub retry: RetryPolicy,
}

impl E2bConfig {
    pub fn from_env() -> E2bResult<Self> {
        Self::new(E2bApiKey::from_env()?)
    }

    pub fn new(api_key: E2bApiKey) -> E2bResult<Self> {
        Ok(Self {
            api_key,
            control_base_url: Url::parse(DEFAULT_CONTROL_URL)
                .expect("the E2B control URL is valid"),
            envd_base_url_override: None,
            envd_port: 49_983,
            username: "user".to_owned(),
            sandbox_timeout_seconds: 300,
            request_timeout: Duration::from_secs(60),
            retry: RetryPolicy::default(),
        })
    }

    pub(crate) fn validate(&self) -> E2bResult<()> {
        if self.envd_port == 0 {
            return Err(E2bError::configuration(
                "E2B envd port must be greater than zero",
            ));
        }
        if self.username.trim().is_empty() {
            return Err(E2bError::configuration(
                "E2B envd username must not be empty",
            ));
        }
        if self.sandbox_timeout_seconds == 0 {
            return Err(E2bError::configuration(
                "E2B sandbox timeout must be greater than zero",
            ));
        }
        if self.sandbox_timeout_seconds > i32::MAX as u64 {
            return Err(E2bError::configuration(
                "E2B sandbox timeout must fit the API's signed 32-bit seconds field",
            ));
        }
        if self.request_timeout.is_zero() {
            return Err(E2bError::configuration(
                "E2B request timeout must be greater than zero",
            ));
        }
        self.retry.validate()
    }

    pub(crate) fn envd_base_url(&self, domain: Option<&str>) -> E2bResult<Url> {
        if let Some(url) = &self.envd_base_url_override {
            return Ok(url.clone());
        }
        let domain = domain.unwrap_or(DEFAULT_ENVD_DOMAIN);
        Url::parse(&format!("https://sandbox.{domain}/")).map_err(|error| {
            E2bError::configuration(format!("E2B returned an invalid sandbox domain: {error}"))
        })
    }
}

/// How the supervisor executable becomes available in an E2B sandbox.
#[derive(Clone, Debug)]
pub enum SupervisorBinary {
    /// The E2B template or snapshot already contains the executable.
    Preinstalled { remote_path: String },
    /// Upload a locally built Linux executable when it is absent or incompatible.
    Upload {
        local_path: PathBuf,
        remote_path: String,
    },
}

impl SupervisorBinary {
    pub(crate) fn remote_path(&self) -> &str {
        match self {
            Self::Preinstalled { remote_path } | Self::Upload { remote_path, .. } => remote_path,
        }
    }
}

/// Target-side paths and identity supplied to `execution-supervisor serve`.
#[derive(Clone, Debug)]
pub struct SupervisorConfig {
    pub binary: SupervisorBinary,
    pub socket_path: String,
    pub state_directory: String,
    pub workspace_path: String,
    pub root_id: RootId,
    pub root_name: String,
    pub read_only: bool,
    pub startup_timeout: Duration,
}

impl Default for SupervisorConfig {
    fn default() -> Self {
        Self {
            binary: SupervisorBinary::Preinstalled {
                remote_path: "/usr/local/bin/execution-supervisor".to_owned(),
            },
            socket_path: "/tmp/agent-pane-execution/supervisor.sock".to_owned(),
            state_directory: "/tmp/agent-pane-execution/state".to_owned(),
            workspace_path: "/home/user".to_owned(),
            root_id: RootId::new("workspace").expect("default root ID is valid"),
            root_name: "Workspace".to_owned(),
            read_only: false,
            startup_timeout: Duration::from_secs(20),
        }
    }
}

/// Everything needed to expose one existing E2B sandbox as an execution runtime.
#[derive(Clone, Debug)]
pub struct E2bRuntimeConfig {
    pub e2b: E2bConfig,
    pub sandbox_id: String,
    pub host_id: ExecutionHostId,
    pub supervisor: SupervisorConfig,
}

impl E2bRuntimeConfig {
    pub(crate) fn validate(&self) -> E2bResult<()> {
        self.e2b.validate()?;
        validate_identifier(&self.sandbox_id, "sandbox ID")?;
        let supervisor = &self.supervisor;
        for (value, name) in [
            (supervisor.binary.remote_path(), "supervisor binary path"),
            (&supervisor.socket_path, "supervisor socket path"),
            (&supervisor.state_directory, "supervisor state directory"),
            (&supervisor.workspace_path, "workspace path"),
        ] {
            if !value.starts_with('/') || value.contains('\0') {
                return Err(E2bError::configuration(format!(
                    "{name} must be an absolute path without null bytes"
                )));
            }
        }
        if supervisor.root_name.trim().is_empty() || supervisor.startup_timeout.is_zero() {
            return Err(E2bError::configuration(
                "supervisor root name and startup timeout must be set",
            ));
        }
        Ok(())
    }
}

pub(crate) fn validate_identifier(value: &str, name: &str) -> E2bResult<()> {
    if value.is_empty() || value.trim() != value || value.contains(['/', '\0']) {
        return Err(E2bError::configuration(format!(
            "E2B {name} must not be blank, contain slashes, null bytes, or surrounding whitespace"
        )));
    }
    Ok(())
}
