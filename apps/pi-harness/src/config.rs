use std::{env, num::ParseIntError, time::Duration};

pub use agent_harness_sdk::{
    AgentControlServiceConfig, AgentServiceConfig, BrokerConfig, HarnessServerConfig,
};
use execution_gateway_client::ExecutionGatewayConfig;
use url::Url;
use uuid::Uuid;

const DEFAULT_AGENT_URL: &str = "http://127.0.0.1:8780";
const DEFAULT_LLM_GATEWAY_URL: &str = "http://127.0.0.1:3000";
const DEFAULT_EXECUTION_GATEWAY_URL: &str = "http://127.0.0.1:8790";

#[derive(Clone, Debug)]
pub struct HarnessConfig {
    pub instance_id: String,
    pub max_concurrent_turns: usize,
    pub broker: BrokerConfig,
    pub agent: AgentServiceConfig,
    pub harness_registration: Option<AgentControlServiceConfig>,
    pub llm_gateway: LlmGatewayServiceConfig,
    pub execution_gateway: ExecutionGatewayConfig,
}

impl HarnessConfig {
    pub fn from_env() -> Result<Self, ConfigError> {
        Self::from_lookup(|name| env::var(name).ok())
    }

    #[must_use]
    pub fn server_config(&self) -> HarnessServerConfig {
        HarnessServerConfig {
            max_concurrent_turns: self.max_concurrent_turns,
            broker: self.broker.clone(),
            agent: self.agent.clone(),
        }
    }

    fn from_lookup<F>(mut lookup: F) -> Result<Self, ConfigError>
    where
        F: FnMut(&str) -> Option<String>,
    {
        let instance_id = validated_id(
            "PI_HARNESS_INSTANCE_ID",
            lookup("PI_HARNESS_INSTANCE_ID")
                .unwrap_or_else(|| format!("pi-harness-{}", Uuid::now_v7())),
        )?;
        let max_concurrent_turns = positive_usize(
            "PI_HARNESS_MAX_CONCURRENT_TURNS",
            lookup("PI_HARNESS_MAX_CONCURRENT_TURNS"),
            20,
        )?;
        let agent_url = http_url(
            "PI_HARNESS_AGENT_URL",
            lookup("PI_HARNESS_AGENT_URL")
                .as_deref()
                .unwrap_or(DEFAULT_AGENT_URL),
        )?;
        let agent_timeout = seconds(
            "PI_HARNESS_AGENT_TIMEOUT_SECONDS",
            lookup("PI_HARNESS_AGENT_TIMEOUT_SECONDS"),
            30,
        )?;
        let harness_registration = optional(
            "PI_HARNESS_AGENT_CONTROL_TOKEN",
            lookup("PI_HARNESS_AGENT_CONTROL_TOKEN"),
        )?
        .map(|control_token| AgentControlServiceConfig {
            base_url: agent_url.clone(),
            control_token,
            request_timeout: agent_timeout,
        });
        let agent = AgentServiceConfig {
            base_url: agent_url,
            harness_token: required("PI_HARNESS_AGENT_TOKEN", lookup("PI_HARNESS_AGENT_TOKEN"))?,
            request_timeout: agent_timeout,
        };
        let llm_gateway = LlmGatewayServiceConfig {
            base_url: http_url(
                "PI_HARNESS_LLM_GATEWAY_URL",
                lookup("PI_HARNESS_LLM_GATEWAY_URL")
                    .as_deref()
                    .unwrap_or(DEFAULT_LLM_GATEWAY_URL),
            )?,
            request_timeout: seconds(
                "PI_HARNESS_LLM_TIMEOUT_SECONDS",
                lookup("PI_HARNESS_LLM_TIMEOUT_SECONDS"),
                36 * 60,
            )?,
        };
        let mut execution_gateway = ExecutionGatewayConfig::new(
            http_url(
                "PI_HARNESS_EXECUTION_GATEWAY_URL",
                lookup("PI_HARNESS_EXECUTION_GATEWAY_URL")
                    .as_deref()
                    .unwrap_or(DEFAULT_EXECUTION_GATEWAY_URL),
            )?,
            required(
                "PI_HARNESS_EXECUTION_GATEWAY_TOKEN",
                lookup("PI_HARNESS_EXECUTION_GATEWAY_TOKEN"),
            )?,
        );
        execution_gateway.request_timeout = seconds(
            "PI_HARNESS_EXECUTION_TIMEOUT_SECONDS",
            lookup("PI_HARNESS_EXECUTION_TIMEOUT_SECONDS"),
            30,
        )?;

        Ok(Self {
            instance_id,
            max_concurrent_turns,
            broker: BrokerConfig {
                url: lookup("PI_HARNESS_NATS_URL")
                    .unwrap_or_else(|| "nats://127.0.0.1:4222".to_owned()),
                command_result_timeout: seconds(
                    "PI_HARNESS_COMMAND_RESULT_TIMEOUT_SECONDS",
                    lookup("PI_HARNESS_COMMAND_RESULT_TIMEOUT_SECONDS"),
                    30,
                )?,
                command_max_retries: positive_u32(
                    "PI_HARNESS_COMMAND_MAX_RETRIES",
                    lookup("PI_HARNESS_COMMAND_MAX_RETRIES"),
                    3,
                )?,
                command_retry_base: millis(
                    "PI_HARNESS_COMMAND_RETRY_BASE_MILLISECONDS",
                    lookup("PI_HARNESS_COMMAND_RETRY_BASE_MILLISECONDS"),
                    250,
                )?,
                command_retry_max: seconds(
                    "PI_HARNESS_COMMAND_RETRY_MAX_SECONDS",
                    lookup("PI_HARNESS_COMMAND_RETRY_MAX_SECONDS"),
                    10,
                )?,
                delivery_retry_delay: seconds(
                    "PI_HARNESS_DELIVERY_RETRY_SECONDS",
                    lookup("PI_HARNESS_DELIVERY_RETRY_SECONDS"),
                    5,
                )?,
                progress_interval: seconds(
                    "PI_HARNESS_PROGRESS_INTERVAL_SECONDS",
                    lookup("PI_HARNESS_PROGRESS_INTERVAL_SECONDS"),
                    30,
                )?,
            },
            agent,
            harness_registration,
            llm_gateway,
            execution_gateway,
        })
    }
}

#[derive(Clone, Debug)]
pub struct LlmGatewayServiceConfig {
    pub base_url: Url,
    pub request_timeout: Duration,
}

fn required(name: &'static str, value: Option<String>) -> Result<String, ConfigError> {
    let value = value.ok_or(ConfigError::Missing(name))?;
    if value.trim().is_empty() {
        Err(ConfigError::Missing(name))
    } else {
        Ok(value)
    }
}
fn optional(name: &'static str, value: Option<String>) -> Result<Option<String>, ConfigError> {
    match value {
        Some(value) if value.trim().is_empty() => Err(ConfigError::Missing(name)),
        other => Ok(other),
    }
}
fn positive_usize(
    name: &'static str,
    value: Option<String>,
    default: usize,
) -> Result<usize, ConfigError> {
    parse_positive(name, value, default)
}
fn positive_u32(
    name: &'static str,
    value: Option<String>,
    default: u32,
) -> Result<u32, ConfigError> {
    parse_positive(name, value, default)
}
fn parse_positive<T>(
    name: &'static str,
    value: Option<String>,
    default: T,
) -> Result<T, ConfigError>
where
    T: std::str::FromStr<Err = ParseIntError> + PartialOrd + Default,
{
    let Some(raw) = value else {
        return Ok(default);
    };
    let parsed = raw
        .parse::<T>()
        .map_err(|source| ConfigError::InvalidInteger {
            name,
            value: raw,
            source,
        })?;
    if parsed > T::default() {
        Ok(parsed)
    } else {
        Err(ConfigError::NotPositive(name))
    }
}
fn seconds(
    name: &'static str,
    value: Option<String>,
    default: u64,
) -> Result<Duration, ConfigError> {
    Ok(Duration::from_secs(parse_positive(name, value, default)?))
}
fn millis(
    name: &'static str,
    value: Option<String>,
    default: u64,
) -> Result<Duration, ConfigError> {
    Ok(Duration::from_millis(parse_positive(name, value, default)?))
}
fn validated_id(name: &'static str, value: String) -> Result<String, ConfigError> {
    if value.is_empty() || value != value.trim() || value.chars().any(char::is_whitespace) {
        Err(ConfigError::InvalidId(name))
    } else {
        Ok(value)
    }
}
fn http_url(name: &'static str, value: &str) -> Result<Url, ConfigError> {
    let mut url = Url::parse(value).map_err(|source| ConfigError::InvalidUrl { name, source })?;
    if !matches!(url.scheme(), "http" | "https")
        || url.host_str().is_none()
        || url.query().is_some()
        || url.fragment().is_some()
    {
        return Err(ConfigError::InvalidHttpUrl(name));
    }
    if !url.path().ends_with('/') {
        url.set_path(&format!("{}/", url.path()));
    }
    Ok(url)
}

#[derive(Debug, thiserror::Error)]
pub enum ConfigError {
    #[error("{0} must be set and non-empty")]
    Missing(&'static str),
    #[error("{name} must be an integer, got {value:?}")]
    InvalidInteger {
        name: &'static str,
        value: String,
        #[source]
        source: ParseIntError,
    },
    #[error("{0} must be greater than zero")]
    NotPositive(&'static str),
    #[error("{0} must be a non-empty identifier without whitespace")]
    InvalidId(&'static str),
    #[error("{name} is not a valid URL")]
    InvalidUrl {
        name: &'static str,
        #[source]
        source: url::ParseError,
    },
    #[error("{0} must be an HTTP(S) base URL without a query or fragment")]
    InvalidHttpUrl(&'static str),
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;

    use super::HarnessConfig;

    fn load(values: &[(&str, &str)]) -> HarnessConfig {
        let values = values.iter().copied().collect::<HashMap<_, _>>();
        HarnessConfig::from_lookup(|name| values.get(name).map(ToString::to_string)).unwrap()
    }

    #[test]
    fn loads_server_defaults() {
        let config = load(&[
            ("PI_HARNESS_AGENT_TOKEN", "agent-secret"),
            ("PI_HARNESS_EXECUTION_GATEWAY_TOKEN", "execution-secret"),
        ]);
        assert!(config.instance_id.starts_with("pi-harness-"));
        assert_eq!(config.max_concurrent_turns, 20);
        assert_eq!(config.broker.url, "nats://127.0.0.1:4222");
        assert_eq!(config.broker.command_max_retries, 3);
        assert!(config.harness_registration.is_none());
    }

    #[test]
    fn loads_broker_and_concurrency_overrides() {
        let config = load(&[
            ("PI_HARNESS_INSTANCE_ID", "pi-a"),
            ("PI_HARNESS_MAX_CONCURRENT_TURNS", "40"),
            ("PI_HARNESS_NATS_URL", "nats://broker:4222"),
            ("PI_HARNESS_AGENT_TOKEN", "agent-secret"),
            ("PI_HARNESS_AGENT_CONTROL_TOKEN", "control-secret"),
            ("PI_HARNESS_EXECUTION_GATEWAY_TOKEN", "execution-secret"),
        ]);
        assert_eq!(config.instance_id, "pi-a");
        assert_eq!(config.max_concurrent_turns, 40);
        assert_eq!(config.broker.url, "nats://broker:4222");
        assert!(config.harness_registration.is_some());
    }
}
