use crate::{db::Database, execution};
use agent_contracts::{
    AGENT_COMMAND_CONSUMER_NAME, CANCELLED_SUBJECT_PATTERN, COMMAND_STREAM_NAME,
    COMMAND_SUBJECT_PATTERN, EVENT_STREAM_NAME, HarnessCommand, RESULT_SUBJECT_PATTERN,
    WORK_STREAM_NAME, WORK_SUBJECT_PATTERN,
};
use async_nats::jetstream::{self, consumer, stream};
use futures_util::StreamExt as _;
use llm_contracts::Validate as _;
use tokio::task::JoinHandle;
use tokio_util::sync::CancellationToken;

#[derive(Clone)]
pub struct Broker {
    client: async_nats::Client,
    context: jetstream::Context,
}
impl Broker {
    pub async fn connect(url: &str) -> Result<Self, BrokerError> {
        let client = async_nats::connect(url).await?;
        Ok(Self {
            client: client.clone(),
            context: jetstream::new(client),
        })
    }
    #[must_use]
    pub fn context(&self) -> jetstream::Context {
        self.context.clone()
    }
    #[must_use]
    pub fn client(&self) -> async_nats::Client {
        self.client.clone()
    }
    pub async fn ensure_topology(&self) -> Result<(), BrokerError> {
        self.context
            .get_or_create_stream(stream::Config {
                name: WORK_STREAM_NAME.to_owned(),
                subjects: vec![WORK_SUBJECT_PATTERN.to_owned()],
                retention: stream::RetentionPolicy::WorkQueue,
                storage: stream::StorageType::File,
                ..Default::default()
            })
            .await
            .map_err(BrokerError::jetstream)?;
        let commands = self
            .context
            .get_or_create_stream(stream::Config {
                name: COMMAND_STREAM_NAME.to_owned(),
                subjects: vec![COMMAND_SUBJECT_PATTERN.to_owned()],
                retention: stream::RetentionPolicy::WorkQueue,
                storage: stream::StorageType::File,
                ..Default::default()
            })
            .await
            .map_err(BrokerError::jetstream)?;
        commands
            .get_or_create_consumer(
                AGENT_COMMAND_CONSUMER_NAME,
                consumer::pull::Config {
                    durable_name: Some(AGENT_COMMAND_CONSUMER_NAME.to_owned()),
                    filter_subject: COMMAND_SUBJECT_PATTERN.to_owned(),
                    ack_policy: consumer::AckPolicy::Explicit,
                    ..Default::default()
                },
            )
            .await
            .map_err(BrokerError::jetstream)?;
        self.context
            .get_or_create_stream(stream::Config {
                name: EVENT_STREAM_NAME.to_owned(),
                subjects: vec![
                    RESULT_SUBJECT_PATTERN.to_owned(),
                    CANCELLED_SUBJECT_PATTERN.to_owned(),
                ],
                retention: stream::RetentionPolicy::Limits,
                storage: stream::StorageType::File,
                ..Default::default()
            })
            .await
            .map_err(BrokerError::jetstream)?;
        Ok(())
    }
    pub async fn spawn_command_consumer(
        &self,
        database: Database,
        shutdown: CancellationToken,
    ) -> Result<JoinHandle<()>, BrokerError> {
        let stream = self
            .context
            .get_stream(COMMAND_STREAM_NAME)
            .await
            .map_err(BrokerError::jetstream)?;
        let consumer = stream
            .get_consumer::<consumer::pull::Config>(AGENT_COMMAND_CONSUMER_NAME)
            .await
            .map_err(BrokerError::jetstream)?;
        let mut messages = consumer.messages().await.map_err(BrokerError::jetstream)?;
        Ok(tokio::spawn(async move {
            loop {
                tokio::select! {
                    () = shutdown.cancelled() => break,
                    next = messages.next() => {
                        let Some(next) = next else { break; };
                        match next {
                            Ok(message) => match serde_json::from_slice::<HarnessCommand>(&message.payload) {
                                Ok(command) if command.validate().is_err() => {
                                    tracing::warn!(command_id = %command.command_id, "discarding invalid harness command");
                                    if let Err(error) = message.ack().await { tracing::warn!(%error, "could not acknowledge invalid harness command"); }
                                }
                                Ok(command) => match execution::apply_harness_command(&database, &command, &message.payload).await {
                                    Ok(result) => { tracing::info!(command_id = %result.command_id, run_id = %result.run_id, "applied harness command"); if let Err(error) = message.ack().await { tracing::warn!(%error, "could not acknowledge harness command"); } }
                                    Err(error) => tracing::warn!(%error, "harness command processing will be retried"),
                                },
                                Err(error) => { tracing::warn!(%error, "discarding invalid harness command JSON"); if let Err(error) = message.ack().await { tracing::warn!(%error, "could not acknowledge invalid harness command"); } }
                            },
                            Err(error) => tracing::warn!(%error, "could not receive harness command"),
                        }
                    }
                }
            }
        }))
    }
}
#[derive(Debug, thiserror::Error)]
pub enum BrokerError {
    #[error(transparent)]
    Connect(#[from] async_nats::ConnectError),
    #[error("JetStream operation failed: {0}")]
    JetStream(String),
}
impl BrokerError {
    fn jetstream(error: impl std::fmt::Display) -> Self {
        Self::JetStream(error.to_string())
    }
}
