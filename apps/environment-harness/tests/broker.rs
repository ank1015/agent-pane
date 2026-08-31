use std::{
    sync::{Arc, Mutex},
    time::Duration,
};

use agent_contracts::{
    AGENT_COMMAND_CONSUMER_NAME, AppliedHarnessCommand, CANCELLED_SUBJECT_PATTERN,
    COMMAND_STREAM_NAME, COMMAND_SUBJECT_PATTERN, EVENT_STREAM_NAME, HARNESS_PROTOCOL_VERSION,
    HarnessCommand, HarnessCommandOutcome, HarnessCommandResult, HarnessOperation,
    RESULT_SUBJECT_PATTERN, RunStatus, SessionMessage, SessionMessageDelivery,
    SessionMessageOrigin, SessionMessagePage, SessionMessagesAppended, TurnRequested,
    WORK_STREAM_NAME, WORK_SUBJECT_PATTERN, result_subject, turn_subject,
};
use agent_harness_sdk::HarnessServer;
use async_nats::jetstream::{self, consumer, stream};
use axum::{
    Json, Router,
    extract::{Path, State},
    routing::{get, post},
};
use chrono::Utc;
use environment_harness::{
    clients::ENVIRONMENT_HARNESS_DESCRIPTOR,
    config::{AgentServiceConfig, BrokerConfig, HarnessConfig, LlmGatewayServiceConfig},
    runtime::EnvironmentRuntime,
};
use execution_gateway_client::ExecutionGatewayConfig;
use execution_runtime::OperationContext;
use futures_util::StreamExt as _;
use serde_json::{Value, json};
use url::Url;
use uuid::Uuid;

#[tokio::test]
#[ignore = "requires ENVIRONMENT_HARNESS_TEST_NATS_URL pointing to an empty JetStream server"]
async fn consumes_a_turn_and_completes_the_command_round_trip_over_jetstream() {
    let nats_url = std::env::var("ENVIRONMENT_HARNESS_TEST_NATS_URL").expect("test NATS URL");
    let nats = async_nats::connect(&nats_url).await.expect("connect NATS");
    let jetstream = jetstream::new(nats);
    ensure_topology(&jetstream).await;

    let fixture = Arc::new(Fixture::new());
    let agent_url = serve(
        Router::new()
            .route(
                "/v1/harness/runs/{run_id}/messages",
                get(agent_messages).post(append_messages),
            )
            .with_state(fixture.clone()),
    )
    .await;
    let llm_url = serve(Router::new().route("/v1/complete", post(llm_complete))).await;
    let config = config(nats_url, agent_url, llm_url);
    let runtime = EnvironmentRuntime::from_config(&config).expect("Environment runtime");
    let server = Arc::new(
        HarnessServer::connect(
            &config.server_config(),
            ENVIRONMENT_HARNESS_DESCRIPTOR,
            runtime,
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
    let request = fixture.turn_request();
    jetstream
        .publish(
            turn_subject("environment"),
            serde_json::to_vec(&request).unwrap().into(),
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
    assert_eq!(command.expected_state_version, 7);
    let HarnessOperation::Complete { final_message_id } = command.operation else {
        panic!("Environment should complete this turn");
    };
    assert_eq!(
        fixture
            .appended_message_id
            .lock()
            .expect("appended message")
            .as_ref(),
        Some(&final_message_id)
    );

    let result = HarnessCommandResult {
        protocol_version: HARNESS_PROTOCOL_VERSION,
        result_id: Uuid::now_v7(),
        command_id: command.command_id,
        emitted_at: Utc::now(),
        run_id: command.run_id,
        outcome: HarnessCommandOutcome::Applied(AppliedHarnessCommand {
            run_state_version: 8,
            current_session_revision: 2,
            run_status: RunStatus::Completed,
        }),
    };
    jetstream
        .publish(
            result_subject("environment"),
            serde_json::to_vec(&result).unwrap().into(),
        )
        .await
        .expect("publish result")
        .await
        .expect("store result");
    command_message.ack().await.expect("ack command");

    let mut work_stream = jetstream
        .get_stream(WORK_STREAM_NAME)
        .await
        .expect("work stream");
    tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            if work_stream.info().await.expect("work info").state.messages == 0 {
                break;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .expect("work acknowledgement deadline");

    shutdown.cancel();
    server_task
        .await
        .expect("server task")
        .expect("clean server shutdown");
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

fn config(nats_url: String, agent_url: Url, llm_url: Url) -> HarnessConfig {
    HarnessConfig {
        instance_id: "environment-broker-test".to_owned(),
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
            base_url: agent_url,
            harness_token: "harness-secret".to_owned(),
            request_timeout: Duration::from_secs(2),
        },
        harness_registration: None,
        llm_gateway: LlmGatewayServiceConfig {
            base_url: llm_url,
            request_timeout: Duration::from_secs(2),
        },
        execution_gateway: ExecutionGatewayConfig::new(
            "http://127.0.0.1:1".parse().unwrap(),
            "execution-secret",
        ),
    }
}

struct Fixture {
    run_id: Uuid,
    session_id: Uuid,
    appended_message_id: Mutex<Option<Uuid>>,
}

impl Fixture {
    fn new() -> Self {
        Self {
            run_id: Uuid::now_v7(),
            session_id: Uuid::now_v7(),
            appended_message_id: Mutex::new(None),
        }
    }

    fn turn_request(&self) -> TurnRequested {
        TurnRequested {
            protocol_version: HARNESS_PROTOCOL_VERSION,
            event_id: Uuid::now_v7(),
            emitted_at: Utc::now(),
            run_id: self.run_id,
            session_id: self.session_id,
            turn_number: 1,
            max_turns: 5,
            expected_state_version: 7,
            harness_id: "environment".to_owned(),
            harness_slug: "environment".to_owned(),
            harness_revision_id: "environment-test".to_owned(),
            resolved_config: json!({
                "provider": "openai",
                "model_id": "gpt-5.6-sol",
                "reasoning_level": "high",
                "project_id": "019d2aa0-0000-7000-8000-000000000010"
            })
            .as_object()
            .unwrap()
            .clone(),
            current_session_revision: 1,
            resume: None,
        }
    }
}

async fn agent_messages(
    State(fixture): State<Arc<Fixture>>,
    Path(run_id): Path<Uuid>,
) -> Json<SessionMessagePage> {
    assert_eq!(run_id, fixture.run_id);
    Json(SessionMessagePage {
        items: vec![SessionMessage {
            session_message_id: Uuid::now_v7(),
            session_id: fixture.session_id,
            revision: 1,
            message: serde_json::from_value(json!({
                "role": "user",
                "id": "user-1",
                "timestamp": 1,
                "content": [{"type": "text", "content": "Finish the task."}]
            }))
            .unwrap(),
            origin: SessionMessageOrigin::External,
            delivery: SessionMessageDelivery::Immediate,
            run_id: None,
            turn_number: None,
            created_at: Utc::now(),
            committed_at: Utc::now(),
        }],
        next_after_revision: None,
    })
}

async fn append_messages(
    State(fixture): State<Arc<Fixture>>,
    Path(run_id): Path<Uuid>,
    Json(body): Json<Value>,
) -> Json<SessionMessagesAppended> {
    assert_eq!(run_id, fixture.run_id);
    assert_eq!(body["expected_state_version"], 7);
    let message_id = Uuid::parse_str(body["messages"][0]["session_message_id"].as_str().unwrap())
        .expect("message ID");
    *fixture
        .appended_message_id
        .lock()
        .expect("appended message") = Some(message_id);
    Json(SessionMessagesAppended {
        items: Vec::new(),
        current_session_revision: 2,
    })
}

async fn llm_complete() -> Json<Value> {
    Json(json!({
        "request_id": Uuid::now_v7(),
        "account_id": Uuid::now_v7(),
        "message": {
            "id": "assistant-1",
            "model": {"provider": "openai", "id": "gpt-5.6-sol"},
            "duration_ms": 1,
            "native_message": {},
            "content": [{"type": "response", "response": {"content": "Done."}}],
            "stop_reason": "stop",
            "timestamp": 1
        }
    }))
}

async fn serve(app: Router) -> Url {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind mock server");
    let address = listener.local_addr().expect("mock server address");
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    format!("http://{address}").parse().unwrap()
}
