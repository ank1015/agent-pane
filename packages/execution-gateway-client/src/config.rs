use std::time::Duration;

use url::Url;

#[derive(Clone)]
pub struct ExecutionGatewayConfig {
    pub base_url: Url,
    pub api_token: String,
    pub request_timeout: Duration,
    pub poll_interval: Duration,
    pub event_page_size: u32,
}

impl ExecutionGatewayConfig {
    #[must_use]
    pub fn new(base_url: Url, api_token: impl Into<String>) -> Self {
        Self {
            base_url: normalize_base_url(base_url),
            api_token: api_token.into(),
            request_timeout: Duration::from_secs(30),
            poll_interval: Duration::from_millis(100),
            event_page_size: 1_000,
        }
    }
}

impl std::fmt::Debug for ExecutionGatewayConfig {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("ExecutionGatewayConfig")
            .field("base_url", &self.base_url)
            .field("api_token", &"[REDACTED]")
            .field("request_timeout", &self.request_timeout)
            .field("poll_interval", &self.poll_interval)
            .field("event_page_size", &self.event_page_size)
            .finish()
    }
}

fn normalize_base_url(mut base_url: Url) -> Url {
    if !base_url.path().ends_with('/') {
        base_url.set_path(&format!("{}/", base_url.path()));
    }
    base_url
}

#[cfg(test)]
mod tests {
    use super::ExecutionGatewayConfig;

    #[test]
    fn normalizes_the_base_url_and_redacts_the_token() {
        let config = ExecutionGatewayConfig::new(
            "https://example.com/gateway".parse().expect("valid URL"),
            "secret-token",
        );

        assert_eq!(config.base_url.as_str(), "https://example.com/gateway/");
        let debug = format!("{config:?}");
        assert!(debug.contains("[REDACTED]"));
        assert!(!debug.contains("secret-token"));
    }
}
