use std::collections::BTreeMap;

use async_trait::async_trait;
use llm_contracts::{
    AssistantMessage, ContentPart, FunctionTool, ImageContent, ImageDetail, ImageSource, LlmError,
    LlmRequest, LlmTransport, Message, MessageId, ModelCost, ModelId, ModelPricing, ModelRef,
    ProviderId, SearchCommands, SearchInput, SearchRequest, SearchRequestOptions, SearchResponse,
    SearchTransport, StopReason, TextContent, TimeOperation, Timestamp, ToolDefinition,
    UrlImageSource, UserMessage, Validate,
};
use serde_json::{Map, json};

fn model_ref() -> ModelRef {
    ModelRef {
        provider: ProviderId::new("openai").expect("valid provider id"),
        id: ModelId::new("gpt-test").expect("valid model id"),
        name: None,
    }
}

fn request() -> LlmRequest {
    LlmRequest {
        model: model_ref(),
        instructions: None,
        messages: Vec::new(),
        tools: Vec::new(),
        provider_options: Map::new(),
        metadata: BTreeMap::new(),
    }
}

#[test]
fn identifiers_reject_empty_values_at_construction_and_deserialization() {
    assert!(ProviderId::new("  ").is_err());
    assert!(serde_json::from_str::<ModelId>(r#"""#).is_err());

    let id = MessageId::new("message-1").expect("valid message id");
    assert_eq!(id.as_str(), "message-1");
    assert_eq!(
        serde_json::to_value(id).expect("serializable id"),
        "message-1"
    );
}

#[test]
fn request_validation_checks_nested_content_and_duplicate_tools() {
    let mut request = request();
    request.messages.push(Message::User(UserMessage {
        id: MessageId::new("user-1").expect("valid message id"),
        timestamp: Timestamp(1),
        content: vec![ContentPart::Image(ImageContent {
            source: ImageSource::Url(UrlImageSource {
                url: "relative/image.png".to_owned(),
            }),
            detail: Some(ImageDetail::High),
            metadata: None,
        })],
    }));
    let tool = ToolDefinition::Function(FunctionTool {
        name: "weather".to_owned(),
        description: "Get the weather".to_owned(),
        parameters: Map::from_iter([("type".to_owned(), json!("object"))]),
        output_schema: None,
        strict: Some(true),
    });
    request.tools = vec![tool.clone(), tool];

    let error = request.validate().expect_err("request must be invalid");
    assert!(
        error
            .issues
            .iter()
            .any(|issue| issue.path.contains("source.url"))
    );
    assert!(
        error
            .issues
            .iter()
            .any(|issue| issue.message.contains("duplicated"))
    );
}

#[test]
fn portable_request_serializes_tagged_messages_and_content() {
    let mut request = request();
    request.instructions = Some("Be concise".to_owned());
    request.messages.push(Message::User(UserMessage {
        id: MessageId::new("user-1").expect("valid message id"),
        timestamp: Timestamp(10),
        content: vec![ContentPart::Text(TextContent {
            content: "Hello".to_owned(),
            metadata: None,
        })],
    }));

    let value = serde_json::to_value(request).expect("serializable request");
    assert_eq!(value["messages"][0]["role"], "user");
    assert_eq!(value["messages"][0]["content"][0]["type"], "text");
    assert_eq!(value["messages"][0]["content"][0]["content"], "Hello");
}

#[test]
fn usage_pricing_rejects_invalid_costs() {
    let pricing = ModelPricing {
        base: ModelCost {
            input: -1.0,
            output: 2.0,
            cache_read: 0.0,
            cache_write: 0.0,
        },
        above: None,
    };

    assert!(pricing.validate().is_err());
}

#[test]
fn search_contract_preserves_commands_and_validates_required_fields() {
    let request = SearchRequest {
        id: "search-1".to_owned(),
        model: "gpt-test".to_owned(),
        reasoning: None,
        input: Some(SearchInput::Text("Rust async traits".to_owned())),
        commands: Some(SearchCommands {
            time: Some(vec![TimeOperation {
                utc_offset: "+05:30".to_owned(),
            }]),
            ..SearchCommands::default()
        }),
        settings: None,
        max_output_tokens: Some(1_024),
    };

    request.validate().expect("valid search request");
    let value = serde_json::to_value(request).expect("serializable search request");
    assert_eq!(value["commands"]["time"][0]["utc_offset"], "+05:30");

    let invalid = SearchRequest {
        id: String::new(),
        model: String::new(),
        reasoning: None,
        input: Some(SearchInput::Text(" ".to_owned())),
        commands: None,
        settings: None,
        max_output_tokens: Some(0),
    };
    assert_eq!(
        invalid
            .validate()
            .expect_err("invalid search request")
            .issues
            .len(),
        4
    );
}

struct CompletionOnly;

#[async_trait]
impl LlmTransport for CompletionOnly {
    async fn complete(&self, _request: LlmRequest) -> Result<AssistantMessage, LlmError> {
        Err(test_error("completion not configured"))
    }
}

struct SearchOnly;

#[async_trait]
impl SearchTransport for SearchOnly {
    async fn search(
        &self,
        _request: SearchRequest,
        _options: SearchRequestOptions,
    ) -> Result<SearchResponse, LlmError> {
        Err(test_error("search not configured"))
    }
}

#[test]
fn completion_and_search_are_independent_provider_extensions() {
    fn assert_completion<T: LlmTransport>() {}
    fn assert_search<T: SearchTransport>() {}

    assert_completion::<CompletionOnly>();
    assert_search::<SearchOnly>();
}

#[test]
fn assistant_contract_retains_provider_native_response() {
    let message = AssistantMessage {
        id: MessageId::new("assistant-1").expect("valid message id"),
        model: model_ref(),
        usage: None,
        duration_ms: 42,
        native_message: json!({"provider_state": "opaque"}),
        content: Vec::new(),
        stop_reason: StopReason::Stop,
        timestamp: Timestamp(100),
    };

    let round_trip: AssistantMessage = serde_json::from_value(
        serde_json::to_value(&message).expect("serializable assistant message"),
    )
    .expect("deserializable assistant message");
    assert_eq!(round_trip.native_message, message.native_message);
}

fn test_error(message: &str) -> LlmError {
    LlmError {
        message: message.to_owned(),
        provider_code: None,
        provider_type: Some("test".to_owned()),
        http_status: None,
        can_retry: false,
        retry_after_ms: None,
        native_error: None,
    }
}
