use std::{
    collections::HashMap,
    sync::{Arc, Mutex},
    time::Duration,
};

use agent_contracts::{
    HARNESS_PROTOCOL_VERSION, HarnessCommand, HarnessCommandResult, HarnessOperation,
    TurnRequested, command_subject,
};
use async_nats::jetstream;
use chrono::Utc;
use execution_runtime::OperationContext;
use tokio::sync::oneshot;
use uuid::Uuid;

use crate::BrokerConfig;

#[derive(Clone, Default)]
pub(crate) struct PendingResults {
    inner: Arc<Mutex<HashMap<Uuid, oneshot::Sender<HarnessCommandResult>>>>,
}

impl PendingResults {
    pub fn register(&self, command_id: Uuid) -> oneshot::Receiver<HarnessCommandResult> {
        let (sender, receiver) = oneshot::channel();
        self.inner
            .lock()
            .expect("pending command results")
            .insert(command_id, sender);
        receiver
    }

    pub fn remove(&self, command_id: Uuid) {
        self.inner
            .lock()
            .expect("pending command results")
            .remove(&command_id);
    }

    pub fn route(&self, result: HarnessCommandResult) {
        if let Some(sender) = self
            .inner
            .lock()
            .expect("pending command results")
            .remove(&result.command_id)
        {
            let _ = sender.send(result);
        }
    }
}

#[derive(Clone)]
pub(crate) struct CommandBus {
    jetstream: jetstream::Context,
    pending: PendingResults,
    result_timeout: Duration,
    max_retries: u32,
    retry_base: Duration,
    retry_max: Duration,
}

impl CommandBus {
    pub fn new(
        jetstream: jetstream::Context,
        pending: PendingResults,
        config: &BrokerConfig,
    ) -> Self {
        Self {
            jetstream,
            pending,
            result_timeout: config.command_result_timeout,
            max_retries: config.command_max_retries,
            retry_base: config.command_retry_base,
            retry_max: config.command_retry_max,
        }
    }

    pub async fn send(
        &self,
        turn: &TurnRequested,
        operation: HarnessOperation,
        operation_context: &OperationContext,
    ) -> Result<HarnessCommandResult, CommandBusError> {
        let command = HarnessCommand {
            protocol_version: HARNESS_PROTOCOL_VERSION,
            command_id: turn.event_id,
            issued_at: Utc::now(),
            run_id: turn.run_id,
            harness_slug: turn.harness_slug.clone(),
            turn_number: turn.turn_number,
            expected_state_version: turn.expected_state_version,
            operation,
        };
        let mut retry = 0;
        loop {
            match self.send_once(&command, operation_context).await {
                Ok(result) => return Ok(result),
                Err(CommandBusError::Cancelled) => return Err(CommandBusError::Cancelled),
                Err(error) if retry < self.max_retries => {
                    retry += 1;
                    tracing::warn!(command_id = %command.command_id, retry, %error, "retrying harness command");
                    let delay = exponential_delay(self.retry_base, self.retry_max, retry);
                    tokio::select! {
                        () = operation_context.cancelled() => return Err(CommandBusError::Cancelled),
                        () = tokio::time::sleep(delay) => {}
                    }
                }
                Err(error) => return Err(error),
            }
        }
    }

    async fn send_once(
        &self,
        command: &HarnessCommand,
        operation: &OperationContext,
    ) -> Result<HarnessCommandResult, CommandBusError> {
        let receiver = self.pending.register(command.command_id);
        let payload = serde_json::to_vec(command).map_err(CommandBusError::Serialize)?;
        let publish = self
            .jetstream
            .publish(command_subject(command.run_id), payload.into())
            .await;
        let acknowledgement = match publish {
            Ok(ack) => ack
                .await
                .map_err(|error| CommandBusError::Broker(error.to_string())),
            Err(error) => Err(CommandBusError::Broker(error.to_string())),
        };
        if let Err(error) = acknowledgement {
            self.pending.remove(command.command_id);
            return Err(error);
        }
        let result = tokio::select! {
            () = operation.cancelled() => Err(CommandBusError::Cancelled),
            result = tokio::time::timeout(self.result_timeout, receiver) => match result {
                Ok(Ok(result)) => Ok(result),
                Ok(Err(_)) => Err(CommandBusError::ResultRouterStopped),
                Err(_) => Err(CommandBusError::ResultTimeout),
            }
        };
        if result.is_err() {
            self.pending.remove(command.command_id);
        }
        result
    }
}

fn exponential_delay(base: Duration, maximum: Duration, retry: u32) -> Duration {
    let multiplier = 1_u32
        .checked_shl(retry.saturating_sub(1))
        .unwrap_or(u32::MAX);
    base.saturating_mul(multiplier).min(maximum)
}

#[derive(Debug, thiserror::Error)]
pub(crate) enum CommandBusError {
    #[error("could not serialize harness command")]
    Serialize(#[source] serde_json::Error),
    #[error("NATS command publication failed: {0}")]
    Broker(String),
    #[error("Agent command result timed out")]
    ResultTimeout,
    #[error("command result router stopped")]
    ResultRouterStopped,
    #[error("turn operation was cancelled")]
    Cancelled,
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use super::exponential_delay;

    #[test]
    fn command_retry_delay_is_exponential_and_bounded() {
        let base = Duration::from_millis(250);
        let maximum = Duration::from_secs(1);

        assert_eq!(exponential_delay(base, maximum, 1), base);
        assert_eq!(
            exponential_delay(base, maximum, 2),
            Duration::from_millis(500)
        );
        assert_eq!(exponential_delay(base, maximum, 3), maximum);
        assert_eq!(exponential_delay(base, maximum, 20), maximum);
    }
}
