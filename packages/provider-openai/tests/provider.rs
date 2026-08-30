use std::collections::BTreeMap;

use llm_contracts::{
    AssistantContent, AssistantMessage, ContentPart, FunctionTool, ImageContent, ImageDetail,
    ImageSource, LlmRequest, Message, MessageId, ModelId, ModelRef, ProviderId, StopReason,
    TextContent, Timestamp, ToolArguments, ToolCallId, ToolDefinition, UserMessage,
};
use provider_openai::{
    OPENAI_MODELS, build_response_request, calculate_usage_cost, convert_response, find_model,
    select_model_pricing,
};
use serde_json::{Map, Value, json};

fn model_ref(id: &str) -> ModelRef {
    ModelRef {
        provider: ProviderId::new("openai").expect("valid provider id"),
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
fn catalog_is_a_strict_allowlist() {
    assert_eq!(OPENAI_MODELS.len(), 3);
    assert!(find_model("gpt-5.6-sol").is_some());

    let error = build_response_request(&request("gpt-unknown"))
        .expect_err("unknown model must be rejected");
    assert_eq!(error.provider_type.as_deref(), Some("invalid_model"));
    assert!(!error.can_retry);
}

#[test]
fn request_is_non_streaming_and_catalog_owned_fields_win() {
    let mut request = request("gpt-5.6-luna");
    request.instructions = Some("Be useful".into());
    request.messages = vec![Message::User(UserMessage {
        id: MessageId::new("user-1").expect("valid id"),
        timestamp: Timestamp(1),
        content: vec![
            ContentPart::Text(TextContent {
                content: "Hello".into(),
                metadata: None,
            }),
            ContentPart::Image(ImageContent {
                source: ImageSource::Url(llm_contracts::UrlImageSource {
                    url: "https://example.com/image.png".into(),
                }),
                detail: Some(ImageDetail::High),
                metadata: None,
            }),
        ],
    })];
    request.provider_options = Map::from_iter([
        ("model".into(), json!("bypass-model")),
        ("stream".into(), json!(true)),
        ("input".into(), json!(["bypass-input"])),
        ("temperature".into(), json!(0.2)),
    ]);

    let body = build_response_request(&request).expect("valid request");

    assert_eq!(body["model"], "gpt-5.6-luna");
    assert_eq!(body["instructions"], "Be useful");
    assert_eq!(body["temperature"], 0.2);
    assert!(body.get("stream").is_none());
    assert_eq!(body["input"][0]["role"], "user");
    assert_eq!(body["input"][0]["content"][0]["type"], "input_text");
    assert_eq!(
        body["input"][0]["content"][1]["image_url"],
        "https://example.com/image.png"
    );
}

#[test]
fn request_merges_hosted_and_portable_tools() {
    let mut request = request("gpt-5.6-luna");
    request
        .provider_options
        .insert("tools".into(), json!([{ "type": "web_search_preview" }]));
    request.tools.push(ToolDefinition::Function(FunctionTool {
        name: "weather".into(),
        description: "Get weather".into(),
        parameters: Map::from_iter([("type".into(), json!("object"))]),
        output_schema: None,
        strict: None,
    }));

    let body = build_response_request(&request).expect("valid request");

    assert_eq!(body["tools"][0]["type"], "web_search_preview");
    assert_eq!(body["tools"][1]["type"], "function");
    assert_eq!(body["tools"][1]["strict"], Value::Null);
}

#[test]
fn replays_openai_native_output_for_follow_up_requests() {
    let native_output = json!([{
        "type": "reasoning",
        "id": "reasoning-1",
        "encrypted_content": "opaque"
    }]);
    let mut request = request("gpt-5.6-luna");
    request.messages.push(Message::Assistant(AssistantMessage {
        id: MessageId::new("assistant-1").expect("valid id"),
        model: model_ref("gpt-5.6-luna"),
        usage: None,
        duration_ms: 1,
        native_message: json!({
            "id": "response-previous",
            "object": "response",
            "output": native_output
        }),
        content: vec![AssistantContent::Thinking {
            thinking_text: "normalized".into(),
        }],
        stop_reason: StopReason::Stop,
        timestamp: Timestamp(1),
    }));

    let body = build_response_request(&request).expect("valid request");
    assert_eq!(body["input"], native_output);
}

#[test]
fn response_preserves_native_and_calculates_every_cost_bucket() {
    let model = find_model("gpt-5.6-luna").expect("catalog model");
    let native = json!({
        "id": "response-1",
        "object": "response",
        "model": "resolved-model-snapshot",
        "status": "completed",
        "output": [
            {
                "type": "reasoning",
                "id": "reasoning-1",
                "summary": [{ "type": "summary_text", "text": "Reasoning" }]
            },
            {
                "type": "message",
                "id": "message-1",
                "content": [{ "type": "output_text", "text": "Answer" }]
            },
            {
                "type": "function_call",
                "id": "call-item-1",
                "call_id": "call-1",
                "name": "weather",
                "arguments": "{\"city\":\"Paris\"}"
            }
        ],
        "usage": {
            "input_tokens": 1_000_000,
            "output_tokens": 1_000_000,
            "input_tokens_details": {
                "cached_tokens": 100_000,
                "cache_write_tokens": 50_000
            }
        },
        "future_field": { "preserved": true }
    });

    let message = convert_response(native.clone(), model, 123, 456).expect("valid response");

    assert_eq!(message.native_message, native);
    assert_eq!(message.model.id.as_str(), "gpt-5.6-luna");
    assert_eq!(message.model.name.as_deref(), Some("GPT-5.6 Luna"));
    assert_eq!(message.stop_reason, StopReason::ToolUse);
    assert!(matches!(
        &message.content[0],
        AssistantContent::Thinking { thinking_text } if thinking_text == "Reasoning"
    ));
    assert!(matches!(
        &message.content[2],
        AssistantContent::ToolCall {
            arguments: ToolArguments::Object(arguments),
            tool_call_id,
            ..
        } if arguments["city"] == "Paris" && tool_call_id.as_str() == "call-1"
    ));

    let usage = message.usage.expect("usage");
    assert_eq!(usage.input, Some(850_000));
    assert_eq!(usage.output, Some(1_000_000));
    assert_eq!(usage.cache_read, Some(100_000));
    assert_eq!(usage.cache_write, Some(50_000));
    let cost = usage.cost.expect("cost");
    assert!((cost.input.expect("input cost") - 0.34).abs() < f64::EPSILON);
    assert!((cost.output.expect("output cost") - 1.8).abs() < f64::EPSILON);
    assert!((cost.cache_read.expect("cache-read cost") - 0.004).abs() < f64::EPSILON);
    assert!((cost.cache_write.expect("cache-write cost") - 0.025).abs() < f64::EPSILON);
    assert!((cost.total - 2.169).abs() < f64::EPSILON);
}

#[test]
fn pricing_threshold_is_strictly_above() {
    let model = find_model("gpt-5.6-sol").expect("catalog model");

    let short = select_model_pricing(model, 272_000);
    assert_eq!(short.input, 4.0);
    assert_eq!(short.cache_read, 0.4);
    assert_eq!(short.cache_write, 5.0);
    assert_eq!(short.output, 20.0);

    let long = select_model_pricing(model, 272_001);
    assert_eq!(long.input, 8.0);
    assert_eq!(long.cache_read, 0.8);
    assert_eq!(long.cache_write, 10.0);
    assert_eq!(long.output, 30.0);
}

#[test]
fn catalog_cost_helper_prices_all_usage_fields() {
    let model = find_model("gpt-5.6-sol").expect("catalog model");
    let usage = llm_contracts::Usage {
        input: Some(1_000_000),
        output: Some(1_000_000),
        cache_read: Some(1_000_000),
        cache_write: Some(1_000_000),
        cost: None,
    };

    let cost = calculate_usage_cost(&usage, model, 1);
    assert_eq!(cost.total, 29.4);
}

#[test]
fn malformed_function_arguments_are_retained_as_text() {
    let model = find_model("gpt-5.6-luna").expect("catalog model");
    let message = convert_response(
        json!({
            "id": "response-1",
            "object": "response",
            "model": "gpt-5.6-luna",
            "output": [{
                "type": "function_call",
                "call_id": "call-1",
                "name": "tool",
                "arguments": "[1]"
            }]
        }),
        model,
        1,
        1,
    )
    .expect("valid response");

    assert!(matches!(
        &message.content[0],
        AssistantContent::ToolCall {
            arguments: ToolArguments::String(value),
            tool_call_id,
            ..
        } if value == "[1]" && tool_call_id == &ToolCallId::new("call-1").expect("valid id")
    ));
}

#[test]
fn maps_incomplete_and_refusal_stop_reasons() {
    let model = find_model("gpt-5.6-luna").expect("catalog model");
    let filtered = convert_response(
        json!({
            "id": "response-filtered",
            "object": "response",
            "model": "gpt-5.6-luna",
            "status": "incomplete",
            "incomplete_details": { "reason": "content_filter" },
            "output": []
        }),
        model,
        1,
        1,
    )
    .expect("valid incomplete response");
    assert_eq!(filtered.stop_reason, StopReason::ContentFilter);

    let refused = convert_response(
        json!({
            "id": "response-refused",
            "object": "response",
            "model": "gpt-5.6-luna",
            "status": "completed",
            "output": [{
                "type": "message",
                "content": [{ "type": "refusal", "refusal": "Cannot help" }]
            }]
        }),
        model,
        1,
        1,
    )
    .expect("valid refusal response");
    assert_eq!(refused.stop_reason, StopReason::Refusal);
}

#[test]
fn failed_native_response_becomes_an_llm_error() {
    let model = find_model("gpt-5.6-luna").expect("catalog model");
    let native = json!({
        "id": "response-failed",
        "object": "response",
        "model": "gpt-5.6-luna",
        "status": "failed",
        "output": [],
        "error": {
            "message": "Provider rejected the request",
            "type": "provider_error",
            "code": "rejected"
        }
    });

    let error = convert_response(native.clone(), model, 1, 1).expect_err("failed response");
    assert_eq!(error.message, "Provider rejected the request");
    assert_eq!(error.provider_code.as_deref(), Some("rejected"));
    assert_eq!(error.provider_type.as_deref(), Some("provider_error"));
    assert_eq!(error.native_error, Some(Box::new(native)));
}
