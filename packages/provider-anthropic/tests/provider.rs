use std::collections::BTreeMap;

use llm_contracts::{
    AssistantContent, AssistantMessage, ContentPart, FunctionTool, LlmRequest, Message, MessageId,
    ModelId, ModelRef, ProviderId, StopReason, TextContent, Timestamp, ToolArguments,
    ToolDefinition, UserMessage,
};
use provider_anthropic::{
    ANTHROPIC_MODELS, build_message_request, calculate_usage_cost, convert_response, find_model,
};
use serde_json::{Map, json};

fn model_ref(id: &str) -> ModelRef {
    ModelRef {
        provider: ProviderId::new("anthropic").expect("valid provider id"),
        id: ModelId::new(id).expect("valid model id"),
        name: None,
    }
}

fn request(model_id: &str) -> LlmRequest {
    LlmRequest {
        model: model_ref(model_id),
        instructions: None,
        messages: Vec::new(),
        tools: Vec::new(),
        provider_options: Map::new(),
        metadata: BTreeMap::new(),
    }
}

#[test]
fn catalog_is_current_and_a_strict_allowlist() {
    assert_eq!(ANTHROPIC_MODELS.len(), 3);
    assert!(find_model("claude-fable-5").is_some());
    assert!(find_model("claude-opus-5").is_some());
    assert!(find_model("claude-sonnet-5").is_some());
    assert!(find_model("claude-opus-4-6").is_none());
    assert!(find_model("claude-sonnet-5-20260801").is_none());

    let error = build_message_request(&request("claude-unknown"))
        .expect_err("unknown model must be rejected");
    assert_eq!(error.provider_type.as_deref(), Some("invalid_model"));
}

#[test]
fn request_is_non_streaming_and_catalog_owned_fields_win() {
    let mut request = request("claude-sonnet-5");
    request.instructions = Some("Be concise".into());
    request.messages.push(Message::User(UserMessage {
        id: MessageId::new("user-1").expect("valid id"),
        timestamp: Timestamp(1),
        content: vec![ContentPart::Text(TextContent {
            content: "Hello".into(),
            metadata: None,
        })],
    }));
    request.provider_options = Map::from_iter([
        ("model".into(), json!("bypass")),
        ("messages".into(), json!([])),
        ("system".into(), json!("bypass")),
        ("stream".into(), json!(true)),
        ("max_tokens".into(), json!(128_000)),
        ("temperature".into(), json!(0)),
    ]);

    let body = build_message_request(&request).expect("valid request");

    assert_eq!(body["model"], "claude-sonnet-5");
    assert_eq!(body["stream"], false);
    assert_eq!(body["max_tokens"], 128_000);
    assert_eq!(body["system"], "Be concise");
    assert_eq!(body["temperature"], 0);
    assert_eq!(body["messages"][0]["role"], "user");

    request
        .provider_options
        .insert("max_tokens".into(), json!(128_001));
    let error = build_message_request(&request).expect_err("catalog output limit must apply");
    assert!(error.message.contains("cannot exceed 128000"));
}

#[test]
fn portable_and_native_tools_are_combined() {
    let mut request = request("claude-opus-5");
    request.tools.push(ToolDefinition::Function(FunctionTool {
        name: "weather".into(),
        description: "Get weather".into(),
        parameters: Map::from_iter([("type".into(), json!("object"))]),
        strict: Some(true),
    }));
    request.provider_options.insert(
        "tools".into(),
        json!([{ "type": "web_search_20250305", "name": "web_search" }]),
    );

    let body = build_message_request(&request).expect("valid tools");

    assert_eq!(body["tools"].as_array().expect("tools").len(), 2);
    assert_eq!(body["tools"][0]["name"], "web_search");
    assert_eq!(body["tools"][1]["name"], "weather");
    assert_eq!(body["tools"][1]["input_schema"]["type"], "object");
    assert_eq!(body["tools"][1]["strict"], true);
}

#[test]
fn native_anthropic_content_is_replayed_without_losing_signatures() {
    let native_content = json!([
        { "type": "thinking", "thinking": "secret", "signature": "opaque-signature" },
        { "type": "text", "text": "Answer" }
    ]);
    let mut request = request("claude-fable-5");
    request.messages.push(Message::Assistant(AssistantMessage {
        id: MessageId::new("msg-old").expect("valid id"),
        model: model_ref("claude-fable-5"),
        usage: None,
        duration_ms: 1,
        native_message: json!({
            "id": "msg-old",
            "type": "message",
            "role": "assistant",
            "model": "claude-fable-5",
            "content": native_content,
            "stop_reason": "end_turn",
            "usage": { "input_tokens": 1, "output_tokens": 1 }
        }),
        content: Vec::new(),
        stop_reason: StopReason::Stop,
        timestamp: Timestamp(1),
    }));

    let body = build_message_request(&request).expect("valid follow-up");
    assert_eq!(body["messages"][0]["content"], native_content);
}

#[test]
fn response_preserves_native_blocks_and_prices_both_cache_ttls() {
    let model = find_model("claude-sonnet-5").expect("catalog model");
    let native = json!({
        "id": "msg_123",
        "type": "message",
        "role": "assistant",
        "model": "claude-sonnet-5",
        "content": [
            { "type": "thinking", "thinking": "Thought", "signature": "signed" },
            { "type": "text", "text": "Answer", "citations": [] },
            { "type": "tool_use", "id": "toolu_1", "name": "weather", "input": { "city": "Paris" } },
            { "type": "server_tool_result", "tool_use_id": "server_1", "content": [] }
        ],
        "stop_reason": "tool_use",
        "stop_sequence": null,
        "usage": {
            "input_tokens": 1_000_000,
            "output_tokens": 1_000_000,
            "cache_read_input_tokens": 1_000_000,
            "cache_creation_input_tokens": 1_000_000,
            "cache_creation": {
                "ephemeral_5m_input_tokens": 600_000,
                "ephemeral_1h_input_tokens": 400_000
            }
        },
        "future_field": { "preserved": true }
    });

    let message = convert_response(native.clone(), model, 123, 456).expect("valid response");

    assert_eq!(message.native_message, native);
    assert_eq!(message.stop_reason, StopReason::ToolUse);
    assert_eq!(message.content.len(), 3);
    assert!(matches!(
        &message.content[0],
        AssistantContent::Thinking { thinking_text } if thinking_text == "Thought"
    ));
    assert!(matches!(
        &message.content[2],
        AssistantContent::ToolCall {
            arguments: ToolArguments::Object(arguments),
            ..
        } if arguments["city"] == "Paris"
    ));

    let usage = message.usage.expect("usage");
    let cost = usage.cost.expect("cost");
    assert_eq!(usage.input, Some(1_000_000));
    assert_eq!(usage.cache_write, Some(1_000_000));
    assert!((cost.input.expect("input") - 2.0).abs() < f64::EPSILON);
    assert!((cost.output.expect("output") - 10.0).abs() < f64::EPSILON);
    assert!((cost.cache_read.expect("read") - 0.2).abs() < f64::EPSILON);
    assert!((cost.cache_write.expect("write") - 3.1).abs() < f64::EPSILON);
    assert!((cost.total - 15.3).abs() < 1e-12);
}

#[test]
fn public_cost_helper_uses_the_default_five_minute_cache_rate() {
    let model = find_model("claude-opus-5").expect("catalog model");
    let usage = llm_contracts::Usage {
        input: Some(1_000_000),
        output: Some(1_000_000),
        cache_read: Some(1_000_000),
        cache_write: Some(1_000_000),
        cost: None,
    };
    let cost = calculate_usage_cost(&usage, model);
    assert!((cost.total - 36.75).abs() < f64::EPSILON);
}
