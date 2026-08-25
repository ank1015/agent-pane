use std::{collections::HashSet, env, fmt, time::Duration};

use execution_gateway_client::ExecutionGatewayConfig;
use thiserror::Error;
use url::Url;
use uuid::Uuid;

const DEFAULT_MAX_CONCURRENT_RUNS: usize = 20;
const DEFAULT_AGENT_URL: &str = "http://127.0.0.1:8780";
const DEFAULT_AGENT_TIMEOUT_SECONDS: u64 = 30;
const DEFAULT_LLM_GATEWAY_URL: &str = "http://127.0.0.1:3000";
const DEFAULT_LLM_GATEWAY_TIMEOUT_SECONDS: u64 = 36 * 60;
const DEFAULT_EXECUTION_GATEWAY_URL: &str = "http://127.0.0.1:8790";
const DEFAULT_EXECUTION_GATEWAY_TIMEOUT_SECONDS: u64 = 30;

const WORKER_INSTANCE_ID: &str = "PI_WORKER_INSTANCE_ID";
const HARNESS_REVISION_IDS: &str = "PI_WORKER_HARNESS_REVISION_IDS";
const MAX_CONCURRENT_RUNS: &str = "PI_WORKER_MAX_CONCURRENT_RUNS";
const AGENT_URL: &str = "PI_WORKER_AGENT_URL";
const AGENT_TOKEN: &str = "PI_WORKER_AGENT_TOKEN";
const AGENT_CONTROL_TOKEN: &str = "PI_WORKER_AGENT_CONTROL_TOKEN";
const AGENT_TIMEOUT_SECONDS: &str = "PI_WORKER_AGENT_TIMEOUT_SECONDS";
const LLM_GATEWAY_URL: &str = "PI_WORKER_LLM_GATEWAY_URL";
const LLM_GATEWAY_TIMEOUT_SECONDS: &str = "PI_WORKER_LLM_TIMEOUT_SECONDS";
const EXECUTION_GATEWAY_URL: &str = "PI_WORKER_EXECUTION_GATEWAY_URL";
const EXECUTION_GATEWAY_TOKEN: &str = "PI_WORKER_EXECUTION_GATEWAY_TOKEN";
const EXECUTION_GATEWAY_TIMEOUT_SECONDS: &str = "PI_WORKER_EXECUTION_TIMEOUT_SECONDS";

#[derive(Clone, Debug)]
pub struct WorkerConfig {
    pub worker_instance_id: String,
    pub supported_harness_revision_ids: Vec<String>,
    pub max_concurrent_runs: usize,
    pub agent: AgentServiceConfig,
    pub harness_registration: Option<AgentControlServiceConfig>,
    pub llm_gateway: LlmGatewayServiceConfig,
    pub execution_gateway: ExecutionGatewayConfig,
}

impl WorkerConfig {
    /// Loads the worker's process-level configuration from environment variables.
    ///
    /// Claim-specific values such as lease tokens, machine IDs, session IDs, and
    /// working directories are deliberately not part of this configuration.
    pub fn from_env() -> Result<Self, ConfigError> {
        Self::from_lookup(|name| env::var(name).ok())
    }

    fn from_lookup<F>(mut lookup: F) -> Result<Self, ConfigError>
    where
        F: FnMut(&str) -> Option<String>,
    {
        let worker_instance_id = validate_worker_instance_id(
            lookup(WORKER_INSTANCE_ID).unwrap_or_else(|| format!("pi-{}", Uuid::now_v7())),
        )?;
        let supported_harness_revision_ids = revision_ids(required(
            HARNESS_REVISION_IDS,
            lookup(HARNESS_REVISION_IDS),
        )?)?;
        let max_concurrent_runs = positive_usize(
            MAX_CONCURRENT_RUNS,
            lookup(MAX_CONCURRENT_RUNS),
            DEFAULT_MAX_CONCURRENT_RUNS,
        )?;

        let agent_base_url = base_url(
            AGENT_URL,
            lookup(AGENT_URL).as_deref().unwrap_or(DEFAULT_AGENT_URL),
        )?;
        let agent_request_timeout = Duration::from_secs(positive_u64(
            AGENT_TIMEOUT_SECONDS,
            lookup(AGENT_TIMEOUT_SECONDS),
            DEFAULT_AGENT_TIMEOUT_SECONDS,
        )?);
        let harness_registration =
            optional(AGENT_CONTROL_TOKEN, lookup(AGENT_CONTROL_TOKEN))?.map(|control_token| {
                AgentControlServiceConfig {
                    base_url: agent_base_url.clone(),
                    control_token,
                    request_timeout: agent_request_timeout,
                }
            });
        let agent = AgentServiceConfig {
            base_url: agent_base_url,
            worker_token: required(AGENT_TOKEN, lookup(AGENT_TOKEN))?,
            request_timeout: agent_request_timeout,
        };

        let llm_gateway = LlmGatewayServiceConfig {
            base_url: base_url(
                LLM_GATEWAY_URL,
                lookup(LLM_GATEWAY_URL)
                    .as_deref()
                    .unwrap_or(DEFAULT_LLM_GATEWAY_URL),
            )?,
            request_timeout: Duration::from_secs(positive_u64(
                LLM_GATEWAY_TIMEOUT_SECONDS,
                lookup(LLM_GATEWAY_TIMEOUT_SECONDS),
                DEFAULT_LLM_GATEWAY_TIMEOUT_SECONDS,
            )?),
        };

        let execution_gateway_url = base_url(
            EXECUTION_GATEWAY_URL,
            lookup(EXECUTION_GATEWAY_URL)
                .as_deref()
                .unwrap_or(DEFAULT_EXECUTION_GATEWAY_URL),
        )?;
        let execution_gateway_token =
            required(EXECUTION_GATEWAY_TOKEN, lookup(EXECUTION_GATEWAY_TOKEN))?;
        let mut execution_gateway =
            ExecutionGatewayConfig::new(execution_gateway_url, execution_gateway_token);
        execution_gateway.request_timeout = Duration::from_secs(positive_u64(
            EXECUTION_GATEWAY_TIMEOUT_SECONDS,
            lookup(EXECUTION_GATEWAY_TIMEOUT_SECONDS),
            DEFAULT_EXECUTION_GATEWAY_TIMEOUT_SECONDS,
        )?);

        Ok(Self {
            worker_instance_id,
            supported_harness_revision_ids,
            max_concurrent_runs,
            agent,
            harness_registration,
            llm_gateway,
            execution_gateway,
        })
    }
}

#[derive(Clone)]
pub struct AgentServiceConfig {
    pub base_url: Url,
    pub worker_token: String,
    pub request_timeout: Duration,
}

#[derive(Clone)]
pub struct AgentControlServiceConfig {
    pub base_url: Url,
    pub control_token: String,
    pub request_timeout: Duration,
}

impl fmt::Debug for AgentControlServiceConfig {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("AgentControlServiceConfig")
            .field("base_url", &self.base_url)
            .field("control_token", &"[REDACTED]")
            .field("request_timeout", &self.request_timeout)
            .finish()
    }
}

impl fmt::Debug for AgentServiceConfig {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("AgentServiceConfig")
            .field("base_url", &self.base_url)
            .field("worker_token", &"[REDACTED]")
            .field("request_timeout", &self.request_timeout)
            .finish()
    }
}

#[derive(Clone, Debug)]
pub struct LlmGatewayServiceConfig {
    pub base_url: Url,
    pub request_timeout: Duration,
}

#[derive(Debug, Error)]
pub enum ConfigError {
    #[error("{name} must be set")]
    Missing { name: &'static str },
    #[error("{name} must not be empty")]
    Empty { name: &'static str },
    #[error("{name} must be a positive integer, got {value:?}")]
    InvalidPositiveInteger { name: &'static str, value: String },
    #[error("{name} is not a valid URL: {source}")]
    InvalidUrl {
        name: &'static str,
        #[source]
        source: url::ParseError,
    },
    #[error("{name} must use http or https, got {scheme:?}")]
    UnsupportedUrlScheme { name: &'static str, scheme: String },
    #[error("{name} must include a host")]
    UrlMissingHost { name: &'static str },
    #[error("{name} must not include a query string or fragment")]
    UrlHasQueryOrFragment { name: &'static str },
    #[error("{WORKER_INSTANCE_ID} must not be empty or have surrounding whitespace")]
    InvalidWorkerInstanceId,
    #[error("{HARNESS_REVISION_IDS} must be a comma-separated list of unique, non-empty IDs")]
    InvalidHarnessRevisionIds,
}

fn required(name: &'static str, value: Option<String>) -> Result<String, ConfigError> {
    let value = value.ok_or(ConfigError::Missing { name })?;
    if value.trim().is_empty() {
        return Err(ConfigError::Empty { name });
    }
    Ok(value)
}

fn optional(name: &'static str, value: Option<String>) -> Result<Option<String>, ConfigError> {
    match value {
        Some(value) if value.trim().is_empty() => Err(ConfigError::Empty { name }),
        value => Ok(value),
    }
}

fn positive_usize(
    name: &'static str,
    value: Option<String>,
    default: usize,
) -> Result<usize, ConfigError> {
    match value {
        Some(value) => value
            .parse::<usize>()
            .ok()
            .filter(|value| *value > 0)
            .ok_or(ConfigError::InvalidPositiveInteger { name, value }),
        None => Ok(default),
    }
}

fn positive_u64(
    name: &'static str,
    value: Option<String>,
    default: u64,
) -> Result<u64, ConfigError> {
    match value {
        Some(value) => value
            .parse::<u64>()
            .ok()
            .filter(|value| *value > 0)
            .ok_or(ConfigError::InvalidPositiveInteger { name, value }),
        None => Ok(default),
    }
}

fn validate_worker_instance_id(value: String) -> Result<String, ConfigError> {
    if value.is_empty() || value != value.trim() {
        return Err(ConfigError::InvalidWorkerInstanceId);
    }
    Ok(value)
}

fn revision_ids(value: String) -> Result<Vec<String>, ConfigError> {
    let mut seen = HashSet::new();
    let mut ids = Vec::new();
    for part in value.split(',') {
        let id = part.trim();
        if id.is_empty() || !seen.insert(id.to_owned()) {
            return Err(ConfigError::InvalidHarnessRevisionIds);
        }
        ids.push(id.to_owned());
    }
    Ok(ids)
}

fn base_url(name: &'static str, value: &str) -> Result<Url, ConfigError> {
    let mut url = Url::parse(value).map_err(|source| ConfigError::InvalidUrl { name, source })?;
    if !matches!(url.scheme(), "http" | "https") {
        return Err(ConfigError::UnsupportedUrlScheme {
            name,
            scheme: url.scheme().to_owned(),
        });
    }
    if url.host_str().is_none() {
        return Err(ConfigError::UrlMissingHost { name });
    }
    if url.query().is_some() || url.fragment().is_some() {
        return Err(ConfigError::UrlHasQueryOrFragment { name });
    }
    if !url.path().ends_with('/') {
        url.set_path(&format!("{}/", url.path()));
    }
    Ok(url)
}

#[cfg(test)]
mod tests {
    use std::{collections::HashMap, time::Duration};

    use super::{
        AGENT_CONTROL_TOKEN, AGENT_TOKEN, AGENT_URL, ConfigError,
        EXECUTION_GATEWAY_TIMEOUT_SECONDS, EXECUTION_GATEWAY_TOKEN, EXECUTION_GATEWAY_URL,
        HARNESS_REVISION_IDS, LLM_GATEWAY_TIMEOUT_SECONDS, LLM_GATEWAY_URL, MAX_CONCURRENT_RUNS,
        WORKER_INSTANCE_ID, WorkerConfig,
    };

    fn load(values: &[(&str, &str)]) -> Result<WorkerConfig, ConfigError> {
        let mut values = values.iter().copied().collect::<HashMap<_, _>>();
        values
            .entry(HARNESS_REVISION_IDS)
            .or_insert("pi-2026-08-25");
        WorkerConfig::from_lookup(|name| values.get(name).map(ToString::to_string))
    }

    #[test]
    fn loads_defaults_and_keeps_gateway_client_defaults() {
        let config = load(&[
            (AGENT_TOKEN, "agent-secret"),
            (EXECUTION_GATEWAY_TOKEN, "execution-secret"),
        ])
        .expect("valid config");

        assert!(config.worker_instance_id.starts_with("pi-"));
        assert_eq!(config.supported_harness_revision_ids, ["pi-2026-08-25"]);
        assert_eq!(config.max_concurrent_runs, 20);
        assert_eq!(config.agent.base_url.as_str(), "http://127.0.0.1:8780/");
        assert_eq!(config.agent.request_timeout, Duration::from_secs(30));
        assert!(config.harness_registration.is_none());
        assert_eq!(
            config.llm_gateway.base_url.as_str(),
            "http://127.0.0.1:3000/"
        );
        assert_eq!(
            config.llm_gateway.request_timeout,
            Duration::from_secs(36 * 60)
        );
        assert_eq!(
            config.execution_gateway.base_url.as_str(),
            "http://127.0.0.1:8790/"
        );
        assert_eq!(
            config.execution_gateway.request_timeout,
            Duration::from_secs(30)
        );
        assert_eq!(
            config.execution_gateway.poll_interval,
            Duration::from_millis(100)
        );
        assert_eq!(config.execution_gateway.event_page_size, 1_000);
    }

    #[test]
    fn loads_overrides() {
        let config = load(&[
            (WORKER_INSTANCE_ID, "pi-worker-a"),
            (HARNESS_REVISION_IDS, "pi-v1, pi-v2"),
            (MAX_CONCURRENT_RUNS, "50"),
            (AGENT_URL, "https://agent.example.test/api"),
            (AGENT_TOKEN, "agent-secret"),
            (AGENT_CONTROL_TOKEN, "agent-control-secret"),
            (LLM_GATEWAY_URL, "https://llm.example.test"),
            (LLM_GATEWAY_TIMEOUT_SECONDS, "900"),
            (
                EXECUTION_GATEWAY_URL,
                "https://execution.example.test/gateway",
            ),
            (EXECUTION_GATEWAY_TOKEN, "execution-secret"),
            (EXECUTION_GATEWAY_TIMEOUT_SECONDS, "45"),
        ])
        .expect("valid config");

        assert_eq!(config.worker_instance_id, "pi-worker-a");
        assert_eq!(config.supported_harness_revision_ids, ["pi-v1", "pi-v2"]);
        assert_eq!(config.max_concurrent_runs, 50);
        assert_eq!(
            config.agent.base_url.as_str(),
            "https://agent.example.test/api/"
        );
        let registration = config.harness_registration.expect("registration config");
        assert_eq!(
            registration.base_url.as_str(),
            "https://agent.example.test/api/"
        );
        assert_eq!(registration.control_token, "agent-control-secret");
        assert_eq!(
            config.llm_gateway.base_url.as_str(),
            "https://llm.example.test/"
        );
        assert_eq!(config.llm_gateway.request_timeout, Duration::from_secs(900));
        assert_eq!(
            config.execution_gateway.base_url.as_str(),
            "https://execution.example.test/gateway/"
        );
        assert_eq!(
            config.execution_gateway.request_timeout,
            Duration::from_secs(45)
        );
    }

    #[test]
    fn requires_both_service_tokens() {
        assert!(matches!(
            load(&[(EXECUTION_GATEWAY_TOKEN, "execution-secret")]),
            Err(ConfigError::Missing { name: AGENT_TOKEN })
        ));
        assert!(matches!(
            load(&[(AGENT_TOKEN, "agent-secret")]),
            Err(ConfigError::Missing {
                name: EXECUTION_GATEWAY_TOKEN
            })
        ));
    }

    #[test]
    fn rejects_invalid_values() {
        assert!(matches!(
            load(&[
                (AGENT_TOKEN, "agent-secret"),
                (EXECUTION_GATEWAY_TOKEN, "execution-secret"),
                (MAX_CONCURRENT_RUNS, "0"),
            ]),
            Err(ConfigError::InvalidPositiveInteger {
                name: MAX_CONCURRENT_RUNS,
                ..
            })
        ));
        assert!(matches!(
            load(&[
                (AGENT_TOKEN, "agent-secret"),
                (EXECUTION_GATEWAY_TOKEN, "execution-secret"),
                (HARNESS_REVISION_IDS, "pi-v1,pi-v1"),
            ]),
            Err(ConfigError::InvalidHarnessRevisionIds)
        ));
        assert!(matches!(
            load(&[
                (AGENT_TOKEN, "agent-secret"),
                (EXECUTION_GATEWAY_TOKEN, "execution-secret"),
                (AGENT_URL, "file:///tmp/agent.sock"),
            ]),
            Err(ConfigError::UnsupportedUrlScheme {
                name: AGENT_URL,
                ..
            })
        ));
        assert!(matches!(
            load(&[
                (AGENT_TOKEN, "agent-secret"),
                (EXECUTION_GATEWAY_TOKEN, "execution-secret"),
                (LLM_GATEWAY_URL, "https://llm.example.test?region=west"),
            ]),
            Err(ConfigError::UrlHasQueryOrFragment {
                name: LLM_GATEWAY_URL
            })
        ));
    }

    #[test]
    fn redacts_tokens_from_debug_output() {
        let config = load(&[
            (AGENT_TOKEN, "agent-secret"),
            (AGENT_CONTROL_TOKEN, "agent-control-secret"),
            (EXECUTION_GATEWAY_TOKEN, "execution-secret"),
        ])
        .expect("valid config");

        let debug = format!("{config:?}");
        assert!(debug.contains("[REDACTED]"));
        assert!(!debug.contains("agent-secret"));
        assert!(!debug.contains("agent-control-secret"));
        assert!(!debug.contains("execution-secret"));
    }
}
