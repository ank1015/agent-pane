use crate::{Error, Result, ServerError};
use chrono::Utc;
use platform_runtime_contracts::ErrorEnvelope;
use rand::Rng;
use reqwest::{Method, RequestBuilder, redirect::Policy};
use serde::de::DeserializeOwned;
use std::time::Duration;
use url::Url;

#[derive(Debug, Clone)]
pub struct ClientConfig {
    /// Per attempt, including reading the response body.
    pub request_timeout: Duration,
    pub connect_timeout: Duration,
    /// Includes the first attempt. Set to one to disable automatic retries.
    pub max_attempts: u32,
    pub retry_base_delay: Duration,
    pub retry_max_delay: Duration,
    pub max_response_bytes: usize,
}
impl Default for ClientConfig {
    fn default() -> Self {
        Self {
            request_timeout: Duration::from_secs(10),
            connect_timeout: Duration::from_secs(3),
            max_attempts: 3,
            retry_base_delay: Duration::from_millis(200),
            retry_max_delay: Duration::from_secs(2),
            max_response_bytes: 16 * 1024 * 1024,
        }
    }
}
pub(crate) struct Transport {
    client: reqwest::Client,
    base: Url,
    config: ClientConfig,
}
impl Transport {
    pub fn new(base: &str, config: ClientConfig) -> Result<Self> {
        let mut base = Url::parse(base).map_err(|_| Error::Invalid("invalid Platform URL"))?;
        let loopback = match base.host() {
            Some(url::Host::Domain("localhost")) => true,
            Some(url::Host::Ipv4(ip)) => ip.is_loopback(),
            Some(url::Host::Ipv6(ip)) => ip.is_loopback(),
            _ => false,
        };
        if !(base.scheme() == "https" || base.scheme() == "http" && loopback)
            || base.host().is_none()
            || !base.username().is_empty()
            || base.password().is_some()
            || base.query().is_some()
            || base.fragment().is_some()
        {
            return Err(Error::Invalid(
                "use HTTPS (HTTP only on loopback), without URL credentials, query, or fragment",
            ));
        }
        if config.request_timeout.is_zero()
            || config.connect_timeout.is_zero()
            || !(1..=10).contains(&config.max_attempts)
            || config.max_response_bytes == 0
            || config.retry_base_delay > config.retry_max_delay
            || config.retry_max_delay > Duration::from_secs(60)
        {
            return Err(Error::Invalid(
                "invalid timeouts, retry bounds, or response limit",
            ));
        }
        if !base.path().ends_with('/') {
            base.set_path(&format!("{}/", base.path()));
        }
        let client = reqwest::Client::builder()
            .redirect(Policy::none())
            .timeout(config.request_timeout)
            .connect_timeout(config.connect_timeout)
            .user_agent(concat!(
                "platform-runtime-client/",
                env!("CARGO_PKG_VERSION")
            ))
            .build()
            .map_err(|_| Error::Invalid("could not build HTTP client"))?;
        Ok(Self {
            client,
            base,
            config,
        })
    }
    pub fn request(&self, method: Method, path: &str) -> Result<RequestBuilder> {
        // Paths are constructed by this crate, never supplied by a caller.
        let url = self
            .base
            .join(path)
            .map_err(|_| Error::Invalid("invalid resource path"))?;
        Ok(self
            .client
            .request(method, url)
            .header("accept", "application/json"))
    }
    pub async fn send<T: DeserializeOwned>(
        &self,
        builder: RequestBuilder,
        retry_safe: bool,
    ) -> Result<T> {
        // Build/serialize once: every automatic retry uses exactly the same body
        // bytes, headers, lease epoch and idempotency key.
        let request = builder
            .build()
            .map_err(|_| Error::Invalid("request could not be serialized"))?;
        if request
            .body()
            .and_then(|b| b.as_bytes())
            .is_some_and(|b| b.len() > 1024 * 1024)
        {
            return Err(Error::Invalid(
                "request exceeds the server's 1 MiB body limit",
            ));
        }
        let attempts = if retry_safe {
            self.config.max_attempts
        } else {
            1
        };
        for attempt in 0..attempts {
            let cloned = request
                .try_clone()
                .ok_or(Error::Invalid("request body is not replayable"))?;
            let (result, retry_after, retryable) = match self.client.execute(cloned).await {
                Err(e) => (
                    Err(Error::Transport {
                        timed_out: e.is_timeout(),
                    }),
                    None,
                    e.is_timeout() || e.is_connect() || e.is_request() || e.is_body(),
                ),
                Ok(mut response) => {
                    let status = response.status();
                    let retryable = matches!(status.as_u16(), 429 | 502 | 503 | 504);
                    let retry_after = response
                        .headers()
                        .get("retry-after")
                        .and_then(|h| h.to_str().ok())
                        .and_then(retry_after);
                    let mut bytes = Vec::new();
                    let mut read_error = None;
                    if response
                        .content_length()
                        .is_some_and(|n| n > self.config.max_response_bytes as u64)
                    {
                        return Err(Error::Protocol("response exceeds configured size limit"));
                    }
                    loop {
                        match response.chunk().await {
                            Ok(Some(chunk)) => {
                                if chunk.len()
                                    > self.config.max_response_bytes.saturating_sub(bytes.len())
                                {
                                    return Err(Error::Protocol(
                                        "response exceeds configured size limit",
                                    ));
                                }
                                bytes.extend_from_slice(&chunk);
                            }
                            Ok(None) => break,
                            Err(e) => {
                                read_error = Some(Error::Transport {
                                    timed_out: e.is_timeout(),
                                });
                                break;
                            }
                        }
                    }
                    if let Some(error) = read_error {
                        (Err(error), retry_after, status.is_success() || retryable)
                    } else if status.is_success() {
                        return serde_json::from_slice(&bytes).map_err(|_| {
                            Error::Protocol("JSON does not match the runtime contract")
                        });
                    } else {
                        let error = serde_json::from_slice::<ErrorEnvelope>(&bytes)
                            .ok()
                            .map(|e| e.error);
                        (
                            Err(Error::Server(ServerError {
                                status: status.as_u16(),
                                error,
                            })),
                            retry_after,
                            retryable,
                        )
                    }
                }
            };
            if !retryable || attempt + 1 == attempts {
                return result;
            }
            // Do not retry sooner than Retry-After. If it exceeds our bounded
            // delay budget, return control to the worker instead of clamping it.
            if retry_after.is_some_and(|d| d > self.config.retry_max_delay) {
                return result;
            }
            let cap = self
                .config
                .retry_base_delay
                .saturating_mul(1 << attempt)
                .min(self.config.retry_max_delay);
            let jitter =
                Duration::from_secs_f64(rand::thread_rng().gen_range(0.0..=cap.as_secs_f64()));
            tokio::time::sleep(jitter.max(retry_after.unwrap_or_default())).await;
        }
        unreachable!("attempt count is validated to be positive")
    }
}
fn retry_after(value: &str) -> Option<Duration> {
    if let Ok(seconds) = value.parse::<u64>() {
        return Some(Duration::from_secs(seconds));
    }
    let date = chrono::DateTime::parse_from_rfc2822(value).ok()?;
    Some(
        (date.with_timezone(&Utc) - Utc::now())
            .to_std()
            .unwrap_or_default(),
    )
}
