mod active_turn;
mod command_bus;

use std::time::Duration;

use agent_contracts::{
    EVENT_STREAM_NAME, HarnessCommandOutcome, HarnessCommandResult, RunCancelled, TurnRequested,
    WORK_STREAM_NAME, cancelled_subject, result_subject, turn_subject,
};
use async_nats::jetstream::{self, consumer, message::AckKind};
use execution_runtime::OperationContext;
use futures_util::StreamExt as _;
use llm_contracts::Validate as _;
use tokio::task::{JoinError, JoinSet};
use uuid::Uuid;

use active_turn::ActiveTurns;
pub use active_turn::{ActiveTurn, ActiveTurnError};
use command_bus::{CommandBus, CommandBusError, PendingResults};

use crate::{
    clients::AgentClient,
    config::HarnessConfig,
    runtime::{PiRuntime, PiRuntimeBuildError, TurnOutcome},
};

const WORK_CONSUMER: &str = "pi-harness-turns-v1";
const PI_HARNESS_ID: &str = "pi";
const PI_HARNESS_SLUG: &str = "pi";

pub struct HarnessServer {
    agent: AgentClient,
    pi: PiRuntime,
    work: consumer::Consumer<consumer::pull::Config>,
    results: consumer::Consumer<consumer::pull::Config>,
    cancellations: consumer::Consumer<consumer::pull::Config>,
    command_bus: CommandBus,
    pending_results: PendingResults,
    active_turns: ActiveTurns,
    max_concurrent_turns: usize,
    delivery_retry_delay: Duration,
    progress_interval: Duration,
}

impl HarnessServer {
    pub async fn connect(config: &HarnessConfig) -> Result<Self, HarnessServerError> {
        let agent = AgentClient::new(config.agent.clone())?;
        let pi = PiRuntime::from_config(config)?;
        let client = async_nats::connect(&config.broker.url).await?;
        let jetstream = jetstream::new(client);
        let work_stream = jetstream
            .get_stream(WORK_STREAM_NAME)
            .await
            .map_err(HarnessServerError::broker)?;
        let work = work_stream
            .get_or_create_consumer(
                WORK_CONSUMER,
                consumer::pull::Config {
                    durable_name: Some(WORK_CONSUMER.to_owned()),
                    filter_subject: turn_subject(PI_HARNESS_SLUG),
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
        let result_consumer_name = format!("pi-results-{}", Uuid::now_v7());
        let results = event_stream
            .create_consumer(consumer::pull::Config {
                name: Some(result_consumer_name),
                filter_subject: result_subject(PI_HARNESS_SLUG),
                deliver_policy: consumer::DeliverPolicy::New,
                ack_policy: consumer::AckPolicy::Explicit,
                inactive_threshold: Duration::from_secs(60),
                ..Default::default()
            })
            .await
            .map_err(HarnessServerError::broker)?;
        let cancellation_consumer_name = format!("pi-cancellations-{}", Uuid::now_v7());
        let cancellations = event_stream
            .create_consumer(consumer::pull::Config {
                name: Some(cancellation_consumer_name),
                filter_subject: cancelled_subject(PI_HARNESS_SLUG),
                deliver_policy: consumer::DeliverPolicy::New,
                ack_policy: consumer::AckPolicy::Explicit,
                inactive_threshold: Duration::from_secs(60),
                ..Default::default()
            })
            .await
            .map_err(HarnessServerError::broker)?;
        let pending_results = PendingResults::default();
        let command_bus = CommandBus::new(jetstream, pending_results.clone(), &config.broker);
        Ok(Self {
            agent,
            pi,
            work,
            results,
            cancellations,
            command_bus,
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
                                agent: self.agent.clone(), pi: self.pi.clone(), command_bus: self.command_bus.clone(),
                                active_turns: self.active_turns.clone(), shutdown: shutdown.clone(),
                                delivery_retry_delay: self.delivery_retry_delay, progress_interval: self.progress_interval,
                            };
                            tasks.spawn(async move { process_delivery(message, context).await });
                        }
                        Err(error) => tracing::warn!(%error, "could not receive Pi turn work"),
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

#[derive(Clone)]
struct TurnTaskContext {
    agent: AgentClient,
    pi: PiRuntime,
    command_bus: CommandBus,
    active_turns: ActiveTurns,
    shutdown: OperationContext,
    delivery_retry_delay: Duration,
    progress_interval: Duration,
}

async fn process_delivery(message: jetstream::Message, context: TurnTaskContext) {
    let request = match serde_json::from_slice::<TurnRequested>(&message.payload) {
        Ok(request)
            if request.validate().is_ok()
                && request.harness_id == PI_HARNESS_ID
                && request.harness_slug == PI_HARNESS_SLUG =>
        {
            request
        }
        Ok(request) => {
            tracing::error!(event_id = %request.event_id, "terminating invalid Pi turn event");
            acknowledge(&message, AckKind::Term).await;
            return;
        }
        Err(error) => {
            tracing::error!(%error, "terminating malformed Pi turn event");
            acknowledge(&message, AckKind::Term).await;
            return;
        }
    };
    let Some(operation) = context.active_turns.register(&request, &context.shutdown) else {
        tracing::warn!(run_id = %request.run_id, "Pi turn is already active in this process");
        acknowledge(&message, AckKind::Nak(Some(context.delivery_retry_delay))).await;
        return;
    };
    let progress_stop = OperationContext::new();
    let progress = spawn_progress(
        message.clone(),
        context.progress_interval,
        progress_stop.clone(),
    );
    let turn = ActiveTurn::new(context.agent, request.clone(), operation.clone());
    let outcome = context.pi.execute(&turn).await;
    let acknowledgement = match outcome {
        Ok(TurnOutcome::Command(command)) => match context
            .command_bus
            .send(&request, command, &operation)
            .await
        {
            Ok(result) => {
                match &result.outcome {
                    HarnessCommandOutcome::Applied(_) | HarnessCommandOutcome::Duplicate(_) => {
                        tracing::info!(run_id = %request.run_id, command_id = %result.command_id, "Pi turn command applied")
                    }
                    HarnessCommandOutcome::Rejected(rejected) => {
                        tracing::warn!(run_id = %request.run_id, code = %rejected.code, message = %rejected.message, "Pi turn command rejected")
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
                tracing::warn!(run_id = %request.run_id, %error, "Pi command delivery exhausted retries");
                AckKind::Nak(Some(context.delivery_retry_delay))
            }
        },
        Ok(TurnOutcome::Cancelled) if context.active_turns.was_cancelled(request.run_id) => {
            AckKind::Ack
        }
        Ok(TurnOutcome::Cancelled) => AckKind::Nak(Some(context.delivery_retry_delay)),
        Ok(TurnOutcome::Stale) => AckKind::Ack,
        Err(error) => {
            tracing::warn!(run_id = %request.run_id, %error, "Pi turn will be redelivered");
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
                () = tokio::time::sleep(interval) => if let Err(error) = message.ack_with(AckKind::Progress).await { tracing::warn!(%error, "could not extend Pi turn acknowledgement window"); }
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
        tracing::error!(%error, "Pi turn task panicked");
    }
}

#[derive(Debug, thiserror::Error)]
pub enum HarnessServerError {
    #[error(transparent)]
    Agent(#[from] crate::clients::AgentClientError),
    #[error(transparent)]
    Pi(#[from] PiRuntimeBuildError),
    #[error(transparent)]
    Connect(#[from] async_nats::ConnectError),
    #[error("NATS JetStream operation failed: {0}")]
    Broker(String),
    #[error("Pi work consumer stopped")]
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
