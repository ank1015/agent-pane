use std::collections::BTreeMap;

use llm_contracts::{
    AssistantContent, ContentPart, LlmRequest, LlmTransport, Message, MessageId, ModelId, ModelRef,
    ProviderId, TextContent, Timestamp, UserMessage,
};
use provider_openrouter::OpenRouterProvider;
use serde_json::{Map, json};

const GEMINI: &str = "google/gemini-3.7-flash";

#[tokio::test]
#[ignore = "requires OPENROUTER_API_KEY and makes a real OpenRouter call"]
async fn completes_a_real_non_streaming_gemini_request() {
    let api_key = std::env::var("OPENROUTER_API_KEY").expect("OPENROUTER_API_KEY must be set");
    let provider = OpenRouterProvider::from_api_key(api_key).expect("valid provider");
    let message = provider
        .complete(minimal_request())
        .await
        .expect("real OpenRouter request succeeds");

    let has_pong = message.content.iter().any(|content| {
        matches!(
            content,
            AssistantContent::Response { response }
                if response.content.trim().eq_ignore_ascii_case("PONG")
        )
    });
    let usage = message.usage.as_ref().expect("response has usage");
    assert!(has_pong);
    assert_eq!(message.model.provider.as_str(), "openrouter");
    assert_eq!(message.model.id.as_str(), GEMINI);
    assert_eq!(message.native_message["object"], "chat.completion");
    assert!(usage.cost.is_some());

    println!(
        "response={}, input={:?}, cache_read={:?}, cache_write={:?}, output={:?}, cost={:?}",
        message.id, usage.input, usage.cache_read, usage.cache_write, usage.output, usage.cost
    );
}

fn minimal_request() -> LlmRequest {
    LlmRequest {
        model: ModelRef {
            provider: ProviderId::new("openrouter").expect("valid provider id"),
            id: ModelId::new(GEMINI).expect("valid model id"),
            name: None,
        },
        instructions: Some("Return exactly the format requested by the user.".into()),
        messages: vec![Message::User(UserMessage {
            id: MessageId::new("live-gemini-3-7-flash").expect("valid message id"),
            timestamp: Timestamp(0),
            content: vec![ContentPart::Text(TextContent {
                content: "Reply with exactly the single word: PONG".into(),
                metadata: None,
            })],
        })],
        tools: Vec::new(),
        provider_options: Map::from_iter([
            ("max_completion_tokens".into(), json!(65_536)),
            ("reasoning".into(), json!({ "effort": "low" })),
        ]),
        metadata: BTreeMap::new(),
    }
}
