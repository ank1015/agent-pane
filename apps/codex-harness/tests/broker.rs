use std::{sync::Arc, time::Duration};

use agent_contracts::{
    AGENT_COMMAND_CONSUMER_NAME, AppliedHarnessCommand, CANCELLED_SUBJECT_PATTERN,
    COMMAND_STREAM_NAME, COMMAND_SUBJECT_PATTERN, EVENT_STREAM_NAME, HARNESS_PROTOCOL_VERSION,
    HarnessCommand, HarnessCommandOutcome, HarnessCommandResult, HarnessOperation,
    RESULT_SUBJECT_PATTERN, RunStatus, TurnRequested, WORK_STREAM_NAME, WORK_SUBJECT_PATTERN,
    result_subject, turn_subject,
};
use agent_harness_sdk::{ActiveTurn, HarnessRuntime, HarnessServer, TurnOutcome};
use async_nats::jetstream::{self, consumer, stream};
use chrono::Utc;
use codex_harness::{
    CODEX_HARNESS_DESCRIPTOR,
    config::{AgentServiceConfig, BrokerConfig, HarnessConfig, LlmGatewayServiceConfig},
};
use execution_runtime::OperationContext;
use futures_util::StreamExt as _;
use serde_json::json;
use uuid::Uuid;

#[tokio::test]
#[ignore = "requires CODEX_HARNESS_TEST_NATS_URL pointing to an empty JetStream server"]
async fn consumes_a_turn_and_completes_the_command_round_trip_over_jetstream() {
    let nats_url = std::env::var("CODEX_HARNESS_TEST_NATS_URL").expect("test NATS URL");
    let nats = async_nats::connect(&nats_url).await.expect("connect NATS");
    let jetstream = jetstream::new(nats);
    ensure_topology(&jetstream).await;

    let config = config(nats_url);
    let server = Arc::new(
        HarnessServer::connect(
            &config.server_config(),
            CODEX_HARNESS_DESCRIPTOR,
            BrokerTestRuntime,
        )
        .await
        .expect("harness server"),
    );
    let shutdown = OperationContext::new();
    let server_task = {
        let server = server.clone();
        let shutdown = shutdown.clone();
        tokio::spawn(async move { server.run(&shutdown).await })
    };

    let command_stream = jetstream
        .get_stream(COMMAND_STREAM_NAME)
        .await
        .expect("command stream");
    let command_consumer = command_stream
        .get_consumer::<consumer::pull::Config>(AGENT_COMMAND_CONSUMER_NAME)
        .await
        .expect("command consumer");
    let mut commands = command_consumer.messages().await.expect("commands");
    let request = turn_request();
    jetstream
        .publish(
            turn_subject("codex"),
            serde_json::to_vec(&request).expect("turn JSON").into(),
        )
        .await
        .expect("publish turn")
        .await
        .expect("store turn");

    let command_message = tokio::time::timeout(Duration::from_secs(5), commands.next())
        .await
        .expect("command deadline")
        .expect("command stream open")
        .expect("command delivery");
    let command: HarnessCommand =
        serde_json::from_slice(&command_message.payload).expect("harness command");
    assert_eq!(command.command_id, request.event_id);
    assert_eq!(command.run_id, request.run_id);
    let HarnessOperation::Fail { failure } = &command.operation else {
        panic!("test runtime should fail the turn explicitly");
    };
    assert_eq!(failure["code"], "broker_round_trip");

    let result = HarnessCommandResult {
        protocol_version: HARNESS_PROTOCOL_VERSION,
        result_id: Uuid::now_v7(),
        command_id: command.command_id,
        emitted_at: Utc::now(),
        run_id: command.run_id,
        outcome: HarnessCommandOutcome::Applied(AppliedHarnessCommand {
            run_state_version: 8,
            current_session_revision: 1,
            run_status: RunStatus::Failed,
        }),
    };
    jetstream
        .publish(
            result_subject("codex"),
            serde_json::to_vec(&result).expect("result JSON").into(),
        )
        .await
        .expect("publish result")
        .await
        .expect("store result");
    command_message.ack().await.expect("ack command");

    shutdown.cancel();
    server_task
        .await
        .expect("server task")
        .expect("clean server shutdown");
}

#[derive(Clone, Copy)]
struct BrokerTestRuntime;

#[async_trait::async_trait]
impl HarnessRuntime for BrokerTestRuntime {
    type Error = std::convert::Infallible;

    async fn execute(&self, _turn: &ActiveTurn) -> Result<TurnOutcome, Self::Error> {
        Ok(TurnOutcome::Command(HarnessOperation::Fail {
            failure: json!({"code": "broker_round_trip"})
                .as_object()
                .expect("failure object")
                .clone(),
        }))
    }
}

async fn ensure_topology(jetstream: &jetstream::Context) {
    jetstream
        .create_stream(stream::Config {
            name: WORK_STREAM_NAME.to_owned(),
            subjects: vec![WORK_SUBJECT_PATTERN.to_owned()],
            retention: stream::RetentionPolicy::WorkQueue,
            storage: stream::StorageType::Memory,
            ..Default::default()
        })
        .await
        .expect("work stream");
    let commands = jetstream
        .create_stream(stream::Config {
            name: COMMAND_STREAM_NAME.to_owned(),
            subjects: vec![COMMAND_SUBJECT_PATTERN.to_owned()],
            retention: stream::RetentionPolicy::WorkQueue,
            storage: stream::StorageType::Memory,
            ..Default::default()
        })
        .await
        .expect("command stream");
    commands
        .create_consumer(consumer::pull::Config {
            durable_name: Some(AGENT_COMMAND_CONSUMER_NAME.to_owned()),
            filter_subject: COMMAND_SUBJECT_PATTERN.to_owned(),
            ack_policy: consumer::AckPolicy::Explicit,
            ..Default::default()
        })
        .await
        .expect("command consumer");
    jetstream
        .create_stream(stream::Config {
            name: EVENT_STREAM_NAME.to_owned(),
            subjects: vec![
                RESULT_SUBJECT_PATTERN.to_owned(),
                CANCELLED_SUBJECT_PATTERN.to_owned(),
            ],
            retention: stream::RetentionPolicy::Limits,
            storage: stream::StorageType::Memory,
            ..Default::default()
        })
        .await
        .expect("event stream");
}

fn config(nats_url: String) -> HarnessConfig {
    HarnessConfig {
        instance_id: "codex-broker-test".to_owned(),
        max_concurrent_turns: 2,
        broker: BrokerConfig {
            url: nats_url,
            command_result_timeout: Duration::from_secs(2),
            command_max_retries: 1,
            command_retry_base: Duration::from_millis(5),
            command_retry_max: Duration::from_millis(10),
            delivery_retry_delay: Duration::from_millis(10),
            progress_interval: Duration::from_millis(100),
        },
        agent: AgentServiceConfig {
            base_url: "http://127.0.0.1:1".parse().expect("Agent URL"),
            harness_token: "harness-secret".to_owned(),
            request_timeout: Duration::from_secs(2),
        },
        harness_registration: None,
        llm_gateway: LlmGatewayServiceConfig {
            base_url: "http://127.0.0.1:1".parse().expect("LLM gateway URL"),
            request_timeout: Duration::from_secs(2),
        },
        execution_gateway: execution_gateway_client::ExecutionGatewayConfig::new(
            "http://127.0.0.1:1".parse().expect("execution gateway URL"),
            "execution-secret".to_owned(),
        ),
        database: codex_harness::config::DatabaseConfig {
            url: "postgres://localhost/codex-harness-test".to_owned(),
            max_connections: 1,
            acquire_timeout: Duration::from_secs(1),
        },
        code_mode_idle_ttl: Duration::from_secs(60),
        timezone: "UTC".to_owned(),
    }
}

fn turn_request() -> TurnRequested {
    TurnRequested {
        protocol_version: HARNESS_PROTOCOL_VERSION,
        event_id: Uuid::now_v7(),
        emitted_at: Utc::now(),
        run_id: Uuid::now_v7(),
        session_id: Uuid::now_v7(),
        turn_number: 1,
        max_turns: 5,
        expected_state_version: 7,
        harness_id: "codex".to_owned(),
        harness_slug: "codex".to_owned(),
        harness_revision_id: "codex-test".to_owned(),
        resolved_config: json!({
            "provider": "openai",
            "model_id": "gpt-5.6-sol",
            "reasoning_level": "high"
        })
        .as_object()
        .expect("config object")
        .clone(),
        current_session_revision: 1,
        resume: None,
    }
}
