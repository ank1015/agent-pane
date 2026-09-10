use std::collections::BTreeMap;

use llm_contracts::{
    AssistantContent, ContentPart, LlmRequest, LlmTransport, Message, MessageId, ModelId, ModelRef,
    ProviderId, TextContent, Timestamp, UserMessage,
};
use provider_openai::{OPENAI_MODELS, OpenAiConfig, OpenAiProvider};
use serde_json::{Map, json};

#[tokio::test]
#[ignore = "requires OPENAI_API_KEY and makes real OpenAI API calls"]
async fn completes_a_real_non_streaming_request_for_every_catalog_model() {
    let api_key = std::env::var("OPENAI_API_KEY").expect("OPENAI_API_KEY must be set");
    let mut config = OpenAiConfig::new(api_key).expect("valid OpenAI configuration");
    if let Ok(organization) = std::env::var("OPENAI_ORGANIZATION") {
        config = config.with_organization(organization);
    }
    if let Ok(project) = std::env::var("OPENAI_PROJECT") {
        config = config.with_project(project);
    }
    let provider = OpenAiProvider::new(config).expect("valid OpenAI provider");

    for model in OPENAI_MODELS {
        let message = provider
            .complete(minimal_request(model.id))
            .await
            .unwrap_or_else(|error| panic!("{} request failed: {error}", model.id));

        let has_text = message.content.iter().any(|content| {
            matches!(
                content,
                AssistantContent::Response { response } if !response.content.is_empty()
            )
        });
        let usage = message
            .usage
            .as_ref()
            .expect("final response includes usage");
        let cost = usage.cost.as_ref().expect("usage includes catalog cost");

        assert!(has_text, "{} response must include text", model.id);
        assert_eq!(message.model.id.as_str(), model.id);
        assert_eq!(message.model.name.as_deref(), Some(model.name));
        assert_eq!(message.native_message["object"], "response");
        assert!(message.native_message.get("output").is_some());
        assert!(cost.total >= 0.0);

        println!(
            "{}: response={}, input={:?}, output={:?}, cost=${:.8}",
            model.id, message.id, usage.input, usage.output, cost.total
        );
    }
}

fn minimal_request(model_id: &str) -> LlmRequest {
    LlmRequest {
        model: ModelRef {
            provider: ProviderId::new("openai").expect("valid provider id"),
            id: ModelId::new(model_id).expect("valid model id"),
            name: None,
        },
        instructions: Some("Follow the user's response format exactly.".into()),
        messages: vec![Message::User(UserMessage {
            id: MessageId::new(format!("live-{model_id}")).expect("valid message id"),
            timestamp: Timestamp(0),
            content: vec![ContentPart::Text(TextContent {
                content: "Reply with exactly the single word: PONG".into(),
                metadata: None,
            })],
        })],
        tools: Vec::new(),
        provider_options: Map::from_iter([
            ("max_output_tokens".into(), json!(64)),
            ("reasoning".into(), json!({ "effort": "low" })),
            ("store".into(), json!(false)),
        ]),
        metadata: BTreeMap::new(),
    }
}
