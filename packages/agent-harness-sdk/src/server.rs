use std::{sync::Arc, time::Duration};

use agent_contracts::{
    EVENT_STREAM_NAME, HARNESS_PROTOCOL_VERSION, HarnessCommandOutcome, HarnessCommandResult,
    HarnessRunEvent, HarnessRunEventData, RunCancelled, TurnRequested, WORK_STREAM_NAME,
    cancelled_subject, result_subject, turn_subject,
};
use async_nats::jetstream::{self, consumer, message::AckKind};
use execution_runtime::OperationContext;
use futures_util::StreamExt as _;
use llm_contracts::Validate as _;
use tokio::task::{JoinError, JoinSet};
use uuid::Uuid;

use crate::{
    ActiveTurn, AgentClient, AgentClientError, HarnessRuntime, HarnessServerConfig, TurnOutcome,
    active_turn::ActiveTurns,
    command_bus::{CommandBus, CommandBusError, PendingResults},
    event_bus::EventBus,
};

/// Static broker identity for one independently deployed harness server.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct HarnessDescriptor {
    pub harness_id: &'static str,
    pub harness_slug: &'static str,
    pub work_consumer: &'static str,
}

impl HarnessDescriptor {
    #[must_use]
    pub const fn new(
        harness_id: &'static str,
        harness_slug: &'static str,
        work_consumer: &'static str,
    ) -> Self {
        Self {
            harness_id,
            harness_slug,
            work_consumer,
        }
    }
}

/// Default server runner used independently by each harness binary.
pub struct HarnessServer<R> {
    descriptor: HarnessDescriptor,
    agent: AgentClient,
    runtime: Arc<R>,
    work: consumer::Consumer<consumer::pull::Config>,
    results: consumer::Consumer<consumer::pull::Config>,
    cancellations: consumer::Consumer<consumer::pull::Config>,
    command_bus: CommandBus,
    event_bus: EventBus,
    pending_results: PendingResults,
    active_turns: ActiveTurns,
    max_concurrent_turns: usize,
    delivery_retry_delay: Duration,
    progress_interval: Duration,
}

impl<R: HarnessRuntime> HarnessServer<R> {
    pub async fn connect(
        config: &HarnessServerConfig,
        descriptor: HarnessDescriptor,
        runtime: R,
    ) -> Result<Self, HarnessServerError> {
        let agent = AgentClient::new(config.agent.clone())?;
        let client = async_nats::connect(&config.broker.url).await?;
        let jetstream = jetstream::new(client);
        let work_stream = jetstream
            .get_stream(WORK_STREAM_NAME)
            .await
            .map_err(HarnessServerError::broker)?;
        let work = work_stream
            .get_or_create_consumer(
                descriptor.work_consumer,
                consumer::pull::Config {
                    durable_name: Some(descriptor.work_consumer.to_owned()),
                    filter_subject: turn_subject(descriptor.harness_slug),
                    ack_policy: consumer::AckPolicy::Explicit,
                    ack_wait: Duration::from_secs(120),
                    max_ack_pending: i64::try_from(config.max_concurrent_turns).unwrap_or(i64::MAX),
                    ..Default::default()
                },
            )
            .await
            .map_err(HarnessServerError::broker)?;
        let event_stream = jetstream
            .get_stream(EVENT_STREAM_NAME)
            .await
            .map_err(HarnessServerError::broker)?;
        let result_consumer_name =
            format!("{}-results-{}", descriptor.harness_slug, Uuid::now_v7());
        let results = event_stream
            .create_consumer(consumer::pull::Config {
                name: Some(result_consumer_name),
                filter_subject: result_subject(descriptor.harness_slug),
                deliver_policy: consumer::DeliverPolicy::New,
                ack_policy: consumer::AckPolicy::Explicit,
                inactive_threshold: Duration::from_secs(60),
                ..Default::default()
            })
            .await
            .map_err(HarnessServerError::broker)?;
        let cancellation_consumer_name = format!(
            "{}-cancellations-{}",
            descriptor.harness_slug,
            Uuid::now_v7()
        );
        let cancellations = event_stream
            .create_consumer(consumer::pull::Config {
                name: Some(cancellation_consumer_name),
                filter_subject: cancelled_subject(descriptor.harness_slug),
                deliver_policy: consumer::DeliverPolicy::New,
                ack_policy: consumer::AckPolicy::Explicit,
                inactive_threshold: Duration::from_secs(60),
                ..Default::default()
            })
            .await
            .map_err(HarnessServerError::broker)?;
        let pending_results = PendingResults::default();
        let event_bus = EventBus::new(jetstream.clone());
        let command_bus = CommandBus::new(jetstream, pending_results.clone(), &config.broker);
        Ok(Self {
            descriptor,
            agent,
            runtime: Arc::new(runtime),
            work,
            results,
            cancellations,
            command_bus,
            event_bus,
            pending_results,
            active_turns: ActiveTurns::default(),
            max_concurrent_turns: config.max_concurrent_turns,
            delivery_retry_delay: config.broker.delivery_retry_delay,
            progress_interval: config.broker.progress_interval,
        })
    }

    pub async fn run(&self, shutdown: &OperationContext) -> Result<(), HarnessServerError> {
        let mut work = self
            .work
            .messages()
            .await
            .map_err(HarnessServerError::broker)?;
        let mut results = self
            .results
            .messages()
            .await
            .map_err(HarnessServerError::broker)?;
        let mut cancellations = self
            .cancellations
            .messages()
            .await
            .map_err(HarnessServerError::broker)?;
        let mut tasks = JoinSet::new();

        loop {
            tokio::select! {
                () = shutdown.cancelled() => break,
                joined = tasks.join_next(), if !tasks.is_empty() => observe_join(joined),
                result = results.next() => {
                    let Some(result) = result else { return Err(HarnessServerError::ResultConsumerStopped); };
                    handle_result(result, &self.pending_results).await;
                },
                cancellation = cancellations.next() => {
                    let Some(cancellation) = cancellation else { return Err(HarnessServerError::CancellationConsumerStopped); };
                    handle_cancellation(cancellation, &self.active_turns).await;
                },
                delivery = work.next(), if tasks.len() < self.max_concurrent_turns => {
                    let Some(delivery) = delivery else { return Err(HarnessServerError::WorkConsumerStopped); };
                    match delivery {
                        Ok(message) => {
                            let context = TurnTaskContext {
                                descriptor: self.descriptor,
                                agent: self.agent.clone(),
                                runtime: self.runtime.clone(),
                                command_bus: self.command_bus.clone(),
                                event_bus: self.event_bus.clone(),
                                active_turns: self.active_turns.clone(),
                                shutdown: shutdown.clone(),
                                delivery_retry_delay: self.delivery_retry_delay,
                                progress_interval: self.progress_interval,
                            };
                            tasks.spawn(async move { process_delivery(message, context).await });
                        }
                        Err(error) => tracing::warn!(%error, "could not receive harness turn work"),
                    }
                }
            }
        }

        while let Some(joined) = tasks.join_next().await {
            observe_join(Some(joined));
        }
        Ok(())
    }
}

struct TurnTaskContext<R> {
    descriptor: HarnessDescriptor,
    agent: AgentClient,
    runtime: Arc<R>,
    command_bus: CommandBus,
    event_bus: EventBus,
    active_turns: ActiveTurns,
    shutdown: OperationContext,
    delivery_retry_delay: Duration,
    progress_interval: Duration,
}

async fn process_delivery<R: HarnessRuntime>(
    message: jetstream::Message,
    context: TurnTaskContext<R>,
) {
    let request = match serde_json::from_slice::<TurnRequested>(&message.payload) {
        Ok(request)
            if request.validate().is_ok()
                && request.harness_id == context.descriptor.harness_id
                && request.harness_slug == context.descriptor.harness_slug =>
        {
            request
        }
        Ok(request) => {
            tracing::error!(event_id = %request.event_id, "terminating invalid harness turn event");
            acknowledge(&message, AckKind::Term).await;
            return;
        }
        Err(error) => {
            tracing::error!(%error, "terminating malformed harness turn event");
            acknowledge(&message, AckKind::Term).await;
            return;
        }
    };
    let Some(operation) = context.active_turns.register(&request, &context.shutdown) else {
        tracing::warn!(run_id = %request.run_id, "harness turn is already active in this process");
        acknowledge(&message, AckKind::Nak(Some(context.delivery_retry_delay))).await;
        return;
    };
    let progress_stop = OperationContext::new();
    let progress = spawn_progress(
        message.clone(),
        context.progress_interval,
        progress_stop.clone(),
    );
    let started = HarnessRunEvent {
        protocol_version: HARNESS_PROTOCOL_VERSION,
        event_id: Uuid::new_v5(&request.event_id, b"turn.started"),
        emitted_at: chrono::Utc::now(),
        run_id: request.run_id,
        harness_slug: request.harness_slug.clone(),
        turn_number: request.turn_number,
        expected_state_version: request.expected_state_version,
        event: HarnessRunEventData::TurnStarted,
    };
    if let Err(error) = context.event_bus.publish(&started).await {
        tracing::warn!(run_id = %request.run_id, %error, "could not publish turn started event");
    }
    let turn = ActiveTurn::with_event_bus(
        context.agent,
        request.clone(),
        operation.clone(),
        context.event_bus,
    );
    let outcome = context.runtime.execute(&turn).await;
    let acknowledgement = match outcome {
        Ok(TurnOutcome::Command(command)) => match context
            .command_bus
            .send(&request, command, &operation)
            .await
        {
            Ok(result) => {
                match &result.outcome {
                    HarnessCommandOutcome::Applied(_) | HarnessCommandOutcome::Duplicate(_) => {
                        tracing::info!(run_id = %request.run_id, command_id = %result.command_id, "harness turn command applied")
                    }
                    HarnessCommandOutcome::Rejected(rejected) => {
                        tracing::warn!(run_id = %request.run_id, code = %rejected.code, message = %rejected.message, "harness turn command rejected")
                    }
                }
                AckKind::Ack
            }
            Err(CommandBusError::Cancelled)
                if context.active_turns.was_cancelled(request.run_id) =>
            {
                AckKind::Ack
            }
            Err(error) => {
                tracing::warn!(run_id = %request.run_id, %error, "harness command delivery exhausted retries");
                AckKind::Nak(Some(context.delivery_retry_delay))
            }
        },
        Ok(TurnOutcome::Cancelled) if context.active_turns.was_cancelled(request.run_id) => {
            AckKind::Ack
        }
        Ok(TurnOutcome::Cancelled) => AckKind::Nak(Some(context.delivery_retry_delay)),
        Ok(TurnOutcome::Stale) => AckKind::Ack,
        Err(error) => {
            tracing::warn!(run_id = %request.run_id, %error, "harness turn will be redelivered");
            AckKind::Nak(Some(context.delivery_retry_delay))
        }
    };
    progress_stop.cancel();
    let _ = progress.await;
    acknowledge(&message, acknowledgement).await;
    context
        .active_turns
        .unregister(request.run_id, request.event_id);
}

fn spawn_progress(
    message: jetstream::Message,
    interval: Duration,
    stop: OperationContext,
) -> tokio::task::JoinHandle<()> {
    tokio::spawn(async move {
        loop {
            tokio::select! {
                () = stop.cancelled() => break,
                () = tokio::time::sleep(interval) => if let Err(error) = message.ack_with(AckKind::Progress).await { tracing::warn!(%error, "could not extend harness turn acknowledgement window"); }
            }
        }
    })
}

async fn handle_result(
    result: Result<jetstream::Message, consumer::pull::MessagesError>,
    pending: &PendingResults,
) {
    match result {
        Ok(message) => {
            match serde_json::from_slice::<HarnessCommandResult>(&message.payload) {
                Ok(result) if result.validate().is_ok() => pending.route(result),
                Ok(result) => {
                    tracing::warn!(command_id = %result.command_id, "discarding invalid Agent command result")
                }
                Err(error) => tracing::warn!(%error, "discarding malformed Agent command result"),
            }
            acknowledge(&message, AckKind::Ack).await;
        }
        Err(error) => tracing::warn!(%error, "could not receive Agent command result"),
    }
}

async fn handle_cancellation(
    result: Result<jetstream::Message, consumer::pull::MessagesError>,
    active: &ActiveTurns,
) {
    match result {
        Ok(message) => {
            match serde_json::from_slice::<RunCancelled>(&message.payload) {
                Ok(event) if event.validate().is_ok() => active.cancel(&event),
                Ok(event) => {
                    tracing::warn!(run_id = %event.run_id, "discarding invalid run cancellation")
                }
                Err(error) => tracing::warn!(%error, "discarding malformed run cancellation"),
            }
            acknowledge(&message, AckKind::Ack).await;
        }
        Err(error) => tracing::warn!(%error, "could not receive run cancellation"),
    }
}

async fn acknowledge(message: &jetstream::Message, kind: AckKind) {
    if let Err(error) = message.ack_with(kind).await {
        tracing::warn!(%error, "could not acknowledge JetStream message");
    }
}

fn observe_join(joined: Option<Result<(), JoinError>>) {
    if let Some(Err(error)) = joined {
        tracing::error!(%error, "harness turn task panicked");
    }
}

#[derive(Debug, thiserror::Error)]
pub enum HarnessServerError {
    #[error(transparent)]
    Agent(#[from] AgentClientError),
    #[error(transparent)]
    Connect(#[from] async_nats::ConnectError),
    #[error("NATS JetStream operation failed: {0}")]
    Broker(String),
    #[error("harness work consumer stopped")]
    WorkConsumerStopped,
    #[error("Agent command result consumer stopped")]
    ResultConsumerStopped,
    #[error("Agent run cancellation consumer stopped")]
    CancellationConsumerStopped,
}

impl HarnessServerError {
    fn broker(error: impl std::fmt::Display) -> Self {
        Self::Broker(error.to_string())
    }
}
