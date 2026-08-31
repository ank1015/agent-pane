use agent::broker::Broker;
use agent_contracts::{
    AGENT_COMMAND_CONSUMER_NAME, COMMAND_STREAM_NAME, COMMAND_SUBJECT_PATTERN,
    HARNESS_RUN_EVENT_SUBJECT_PATTERN, RUN_INPUT_SUBJECT_PATTERN,
};

#[tokio::test]
#[ignore = "requires AGENT_TEST_NATS_URL"]
async fn commands_and_progress_share_one_ordered_input_consumer() {
    let url = std::env::var("AGENT_TEST_NATS_URL").expect("AGENT_TEST_NATS_URL must be set");
    let broker = Broker::connect(&url).await.expect("broker connection");
    broker.ensure_topology().await.expect("Agent topology");

    let mut stream = broker
        .context()
        .get_stream(COMMAND_STREAM_NAME)
        .await
        .expect("command stream");
    let info = stream.info().await.expect("command stream info");
    assert!(
        info.config
            .subjects
            .contains(&COMMAND_SUBJECT_PATTERN.to_owned())
    );
    assert!(
        info.config
            .subjects
            .contains(&HARNESS_RUN_EVENT_SUBJECT_PATTERN.to_owned())
    );

    let consumer = stream
        .get_consumer::<async_nats::jetstream::consumer::pull::Config>(AGENT_COMMAND_CONSUMER_NAME)
        .await
        .expect("run input consumer");
    assert_eq!(
        consumer.cached_info().config.filter_subject,
        RUN_INPUT_SUBJECT_PATTERN
    );
}
