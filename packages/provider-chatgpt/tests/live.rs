use std::collections::BTreeMap;

use llm_contracts::{
    AssistantContent, ContentPart, LlmRequest, LlmTransport, Message, MessageId, ModelId, ModelRef,
    ProviderId, TextContent, Timestamp, UserMessage,
};
use provider_chatgpt::{CHATGPT_MODELS, ChatGptProvider};
use serde_json::{Map, json};

#[tokio::test]
#[ignore = "requires CHATGPT_ACCESS_TOKEN and CHATGPT_ACCOUNT_ID and makes real ChatGPT calls"]
async fn completes_a_real_request_for_every_catalog_model() {
    let access_token =
        std::env::var("CHATGPT_ACCESS_TOKEN").expect("CHATGPT_ACCESS_TOKEN must be set");
    let account_id = std::env::var("CHATGPT_ACCOUNT_ID").expect("CHATGPT_ACCOUNT_ID must be set");
    let provider =
        ChatGptProvider::from_credentials(access_token, account_id).expect("valid provider");

    for model in CHATGPT_MODELS {
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
        let usage = message.usage.as_ref().expect("terminal response has usage");

        assert!(has_pong, "{} must return PONG", model.id);
        assert_eq!(message.model.provider.as_str(), "chatgpt");
        assert_eq!(message.model.id.as_str(), model.id);
        assert_eq!(message.native_message["type"], "chatgpt_response_stream");
        assert!(message.native_message["events"].as_array().is_some());
        assert!(message.native_message["output"].as_array().is_some());
        assert!(usage.cost.is_some());

        println!(
            "{}: response={}, input={:?}, output={:?}, native_events={}",
            model.id,
            message.id,
            usage.input,
            usage.output,
            message.native_message["events"]
                .as_array()
                .map_or(0, Vec::len)
        );
    }
}

fn minimal_request(model_id: &str) -> LlmRequest {
    LlmRequest {
        model: ModelRef {
            provider: ProviderId::new("chatgpt").expect("valid provider id"),
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
        provider_options: Map::from_iter([
            (
                "reasoning".into(),
                json!({ "effort": "low", "summary": "auto" }),
            ),
            ("text".into(), json!({ "verbosity": "low" })),
        ]),
        metadata: BTreeMap::new(),
    }
}
