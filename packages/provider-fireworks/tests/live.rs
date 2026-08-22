use std::collections::BTreeMap;

use llm_contracts::{
    AssistantContent, ContentPart, LlmRequest, LlmTransport, Message, MessageId, ModelId, ModelRef,
    ProviderId, TextContent, Timestamp, UserMessage,
};
use provider_fireworks::{FIREWORKS_MODELS, FireworksProvider};
use serde_json::{Map, json};

#[tokio::test]
#[ignore = "requires FIREWORKS_API_KEY and makes real Fireworks calls"]
async fn completes_a_real_non_streaming_request_for_every_catalog_model() {
    let api_key = std::env::var("FIREWORKS_API_KEY").expect("FIREWORKS_API_KEY must be set");
    let provider = FireworksProvider::from_api_key(api_key).expect("valid provider");

    for model in FIREWORKS_MODELS {
        let message = provider
            .complete(minimal_request(model.id))
            .await
            .unwrap_or_else(|error| panic!("{} request failed: {error}", model.id));
        let has_pong = message.content.iter().any(|content| {
            matches!(
                content,
                AssistantContent::Response { response }
                    if response.content.trim().eq_ignore_ascii_case("PONG")
            )
        });
        let usage = message.usage.as_ref().expect("response has usage");

        assert!(has_pong, "{} must return PONG", model.id);
        assert_eq!(message.model.provider.as_str(), "fireworks");
        assert_eq!(message.model.id.as_str(), model.id);
        assert_eq!(message.native_message["object"], "chat.completion");
        assert!(message.native_message["choices"].as_array().is_some());
        assert!(usage.cost.is_some());

        println!(
            "{}: response={}, input={:?}, cache={:?}, output={:?}",
            model.id, message.id, usage.input, usage.cache_read, usage.output
        );
    }
}

fn minimal_request(model_id: &str) -> LlmRequest {
    LlmRequest {
        model: ModelRef {
            provider: ProviderId::new("fireworks").expect("valid provider id"),
            id: ModelId::new(model_id).expect("valid model id"),
            name: None,
        },
        instructions: Some("Return exactly the format requested by the user.".into()),
        messages: vec![Message::User(UserMessage {
            id: MessageId::new(format!("live-{model_id}")).expect("valid message id"),
            timestamp: Timestamp(0),
            content: vec![ContentPart::Text(TextContent {
                content: "Reply with exactly the single word: PONG".into(),
                metadata: None,
            })],
        })],
        tools: Vec::new(),
        provider_options: Map::from_iter([("max_tokens".into(), json!(1_000_000))]),
        metadata: BTreeMap::new(),
    }
}
