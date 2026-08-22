use std::collections::BTreeMap;

use llm_contracts::{
    AssistantContent, ContentPart, LlmRequest, LlmTransport, Message, MessageId, ModelId, ModelRef,
    ProviderId, TextContent, Timestamp, UserMessage,
};
use provider_anthropic::{
    ANTHROPIC_MODELS, AnthropicConfig, AnthropicProvider, DEFAULT_ANTHROPIC_TIMEOUT,
};
use reqwest::header::{HeaderMap, HeaderName, HeaderValue};
use serde_json::{Map, json};

#[tokio::test]
#[ignore = "requires ANTHROPIC_API_KEY and makes real Anthropic calls"]
async fn completes_a_real_large_max_tokens_non_streaming_request_for_every_catalog_model() {
    let api_key = std::env::var("ANTHROPIC_API_KEY").expect("ANTHROPIC_API_KEY must be set");
    let provider = live_provider(api_key);

    for model in ANTHROPIC_MODELS {
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
        assert_eq!(message.model.provider.as_str(), "anthropic");
        assert_eq!(message.model.id.as_str(), model.id);
        assert_eq!(message.native_message["type"], "message");
        assert!(message.native_message["content"].as_array().is_some());
        assert!(usage.cost.is_some());

        println!(
            "{}: response={}, input={:?}, cache_read={:?}, cache_write={:?}, output={:?}",
            model.id, message.id, usage.input, usage.cache_read, usage.cache_write, usage.output
        );
    }
}

fn live_provider(api_key: String) -> AnthropicProvider {
    let configured_base_url = std::env::var("ANTHROPIC_BASE_URL").ok();
    let uses_aws_platform = configured_base_url
        .as_deref()
        .is_some_and(|base_url| base_url.contains("aws-external-anthropic."));
    let mut config = if uses_aws_platform {
        AnthropicConfig::from_bearer_token(api_key).expect("valid AWS bearer token")
    } else {
        AnthropicConfig::new(api_key).expect("valid provider config")
    };
    if let Some(base_url) = configured_base_url {
        let base_url = base_url.trim_end_matches('/');
        let base_url = if base_url.ends_with("/v1") {
            base_url.to_owned()
        } else {
            format!("{base_url}/v1")
        };
        config = config
            .with_base_url(base_url)
            .expect("valid ANTHROPIC_BASE_URL");
    }

    let mut headers = HeaderMap::new();
    if let Ok(custom_headers) = std::env::var("ANTHROPIC_CUSTOM_HEADERS") {
        for header in custom_headers
            .lines()
            .filter(|line| !line.trim().is_empty())
        {
            let (name, value) = header
                .split_once(':')
                .expect("ANTHROPIC_CUSTOM_HEADERS entries must use name:value");
            let name =
                HeaderName::from_bytes(name.trim().as_bytes()).expect("valid custom header name");
            let mut value = HeaderValue::from_str(value.trim()).expect("valid custom header value");
            value.set_sensitive(true);
            headers.insert(name, value);
        }
    }
    let client = reqwest::Client::builder()
        .default_headers(headers)
        .timeout(DEFAULT_ANTHROPIC_TIMEOUT)
        .build()
        .expect("valid HTTP client");
    AnthropicProvider::with_client(config, client)
}

fn minimal_request(model_id: &str) -> LlmRequest {
    LlmRequest {
        model: ModelRef {
            provider: ProviderId::new("anthropic").expect("valid provider id"),
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
        provider_options: Map::from_iter([("max_tokens".into(), json!(128_000))]),
        metadata: BTreeMap::new(),
    }
}
