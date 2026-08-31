use std::time::Duration;

use execution_runtime::OperationContext;
use llm_contracts::{AssistantMessage, LlmRequest};
use uuid::Uuid;

use crate::clients::{LlmGatewayClient, LlmGatewayClientError};

const DEFAULT_MAX_RETRY_DELAY: Duration = Duration::from_secs(60);

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct RetryPolicy {
    pub max_retries: u32,
    pub base_delay: Duration,
    pub max_retry_delay: Duration,
}

impl RetryPolicy {
    #[must_use]
    pub const fn environment_default() -> Self {
        Self {
            max_retries: 3,
            base_delay: Duration::from_secs(2),
            max_retry_delay: DEFAULT_MAX_RETRY_DELAY,
        }
    }

    pub(super) fn delay(self, retry: u32, requested: Option<Duration>) -> Duration {
        let multiplier = 1_u32
            .checked_shl(retry.saturating_sub(1))
            .unwrap_or(u32::MAX);
        let exponential = self.base_delay.saturating_mul(multiplier);
        requested
            .map_or(exponential, |requested| requested.max(exponential))
            .min(self.max_retry_delay)
    }
}

impl Default for RetryPolicy {
    fn default() -> Self {
        Self::environment_default()
    }
}

pub(super) async fn complete_with_retry(
    client: &LlmGatewayClient,
    account_id: Option<Uuid>,
    request: &LlmRequest,
    operation: &OperationContext,
    policy: RetryPolicy,
) -> Result<AssistantMessage, LlmGatewayClientError> {
    let mut retries = 0;
    loop {
        match client.complete(account_id, request, operation).await {
            Ok(message) => return Ok(message),
            Err(error)
                if retries < policy.max_retries
                    && error.retryable()
                    && !error.is_context_overflow() =>
            {
                retries += 1;
                let delay = policy.delay(retries, error.retry_after());
                tokio::select! {
                    () = operation.cancelled() => {
                        return Err(LlmGatewayClientError::Cancelled);
                    }
                    () = tokio::time::sleep(delay) => {}
                }
            }
            Err(error) => return Err(error),
        }
    }
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use super::RetryPolicy;

    #[test]
    fn matches_environment_default_backoff_and_honors_bounded_provider_delays() {
        let policy = RetryPolicy::environment_default();

        assert_eq!(policy.delay(1, None), Duration::from_secs(2));
        assert_eq!(policy.delay(2, None), Duration::from_secs(4));
        assert_eq!(policy.delay(3, None), Duration::from_secs(8));
        assert_eq!(
            policy.delay(1, Some(Duration::from_secs(12))),
            Duration::from_secs(12)
        );
        assert_eq!(
            policy.delay(1, Some(Duration::from_secs(120))),
            Duration::from_secs(60)
        );
    }
}
