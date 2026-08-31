use agent_contracts::{HarnessRunEvent, harness_run_event_subject};
use async_nats::{HeaderMap, jetstream};
use llm_contracts::Validate as _;

#[derive(Clone)]
pub(crate) struct EventBus {
    jetstream: jetstream::Context,
}

impl EventBus {
    pub const fn new(jetstream: jetstream::Context) -> Self {
        Self { jetstream }
    }

    pub async fn publish(&self, event: &HarnessRunEvent) -> Result<(), EventPublishError> {
        event.validate()?;
        let payload = serde_json::to_vec(event).map_err(EventPublishError::Serialize)?;
        let mut headers = HeaderMap::new();
        headers.insert("Nats-Msg-Id", event.event_id.to_string());
        self.jetstream
            .publish_with_headers(
                harness_run_event_subject(event.run_id),
                headers,
                payload.into(),
            )
            .await
            .map_err(|error| EventPublishError::Broker(error.to_string()))?
            .await
            .map_err(|error| EventPublishError::Broker(error.to_string()))?;
        Ok(())
    }
}

#[derive(Debug, thiserror::Error)]
pub enum EventPublishError {
    #[error(transparent)]
    Validation(#[from] llm_contracts::ValidationError),
    #[error("could not serialize harness run event")]
    Serialize(#[source] serde_json::Error),
    #[error("NATS harness run event publication failed: {0}")]
    Broker(String),
    #[error("run event publishing is unavailable outside a harness server")]
    Unavailable,
}
