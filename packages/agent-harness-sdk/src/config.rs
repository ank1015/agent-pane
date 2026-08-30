use std::{fmt, time::Duration};

use url::Url;

#[derive(Clone, Debug)]
pub struct HarnessServerConfig {
    pub max_concurrent_turns: usize,
    pub broker: BrokerConfig,
    pub agent: AgentServiceConfig,
}

#[derive(Clone, Debug)]
pub struct BrokerConfig {
    pub url: String,
    pub command_result_timeout: Duration,
    pub command_max_retries: u32,
    pub command_retry_base: Duration,
    pub command_retry_max: Duration,
    pub delivery_retry_delay: Duration,
    pub progress_interval: Duration,
}

#[derive(Clone)]
pub struct AgentServiceConfig {
    pub base_url: Url,
    pub harness_token: String,
    pub request_timeout: Duration,
}

#[derive(Clone)]
pub struct AgentControlServiceConfig {
    pub base_url: Url,
    pub control_token: String,
    pub request_timeout: Duration,
}

impl fmt::Debug for AgentServiceConfig {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("AgentServiceConfig")
            .field("base_url", &self.base_url)
            .field("harness_token", &"[REDACTED]")
            .field("request_timeout", &self.request_timeout)
            .finish()
    }
}

impl fmt::Debug for AgentControlServiceConfig {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("AgentControlServiceConfig")
            .field("base_url", &self.base_url)
            .field("control_token", &"[REDACTED]")
            .field("request_timeout", &self.request_timeout)
            .finish()
    }
}
