use std::collections::BTreeMap;

use llm_contracts::{
    AssistantContent, AssistantMessage, ContentPart, FunctionTool, LlmRequest, Message, MessageId,
    ModelId, ModelRef, ProviderId, StopReason, TextContent, Timestamp, ToolArguments, ToolCallId,
    ToolDefinition, UserMessage,
};
use provider_openrouter::{
    OPENROUTER_MODELS, build_chat_completion_request, calculate_usage_cost, convert_response,
    find_model,
};
use serde_json::{Map, json};

const GEMINI: &str = "google/gemini-3.7-flash";

fn model_ref(id: &str) -> ModelRef {
    ModelRef {
        provider: ProviderId::new("openrouter").expect("valid provider id"),
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
fn catalog_contains_only_curated_models() {
    assert_eq!(OPENROUTER_MODELS.len(), 5);
    let model = find_model(GEMINI).expect("curated model");
    assert_eq!(model.name, "Google: Gemini 3.7 Flash");
    assert_eq!(model.canonical_slug, "google/gemini-3.7-flash-20260813");
    assert_eq!(model.context_window, 1_048_576);
    assert_eq!(model.max_tokens, 65_536);
    assert_eq!(model.pricing.base.input, 0.375);
    assert_eq!(model.pricing.base.output, 1.875);
    assert!(model.supported_parameters.contains(&"reasoning"));
    let expected = [
        ("meta/muse-spark-1.2-contributor", 1_048_576, 0.10, 0.20),
        ("deepseek/deepseek-v4-flash-vision-exp", 384_000, 0.22, 0.66),
        ("z-ai/glm-5.3", 131_072, 1.40, 4.40),
        ("stealth/ox-alpha", 131_072, 0.0, 0.0),
    ];
    for (id, max_tokens, input_price, output_price) in expected {
        let model = find_model(id).unwrap_or_else(|| panic!("missing curated model {id}"));
        assert_eq!(model.context_window, 1_048_576);
        assert_eq!(model.max_tokens, max_tokens);
        assert_eq!(model.pricing.base.input, input_price);
        assert_eq!(model.pricing.base.output, output_price);
        assert!(model.supported_parameters.contains(&"tools"));
    }
    assert!(find_model("openrouter/free").is_none());

    let error = build_chat_completion_request(&request("openrouter/free"))
        .expect_err("unknown model must be rejected");
    assert_eq!(error.provider_type.as_deref(), Some("invalid_model"));
}

#[test]
fn request_is_non_streaming_and_provider_owned_fields_win() {
    let mut request = request(GEMINI);
    request.instructions = Some("Be concise".into());
    request.messages.push(Message::User(UserMessage {
        id: MessageId::new("user-1").expect("valid id"),
        timestamp: Timestamp(1),
        content: vec![ContentPart::Text(TextContent {
            content: "Say hello".into(),
            metadata: None,
        })],
    }));
    request.provider_options = Map::from_iter([
        ("model".into(), json!("bypass")),
        ("messages".into(), json!([])),
        ("stream".into(), json!(true)),
        ("stream_options".into(), json!({ "include_usage": false })),
        ("n".into(), json!(8)),
        ("max_completion_tokens".into(), json!(65_536)),
        ("reasoning".into(), json!({ "effort": "low" })),
    ]);

    let body = build_chat_completion_request(&request).expect("valid request");
    assert_eq!(body["model"], GEMINI);
    assert_eq!(body["stream"], false);
    assert!(body.get("n").is_none());
    assert!(body.get("stream_options").is_none());
    assert_eq!(body["max_completion_tokens"], 65_536);
    assert_eq!(body["reasoning"]["effort"], "low");
    assert_eq!(body["messages"][0]["role"], "system");
    assert_eq!(body["messages"][1]["role"], "user");
}

#[test]
fn request_validates_output_limit_streaming_only_options_and_fallbacks() {
    let mut request = request(GEMINI);
    request
        .provider_options
        .insert("max_tokens".into(), json!(65_537));
    assert!(
        build_chat_completion_request(&request)
            .expect_err("above catalog limit")
            .message
            .contains("cannot exceed 65536")
    );

    request.provider_options = Map::from_iter([
        ("max_tokens".into(), json!(16)),
        ("max_completion_tokens".into(), json!(16)),
    ]);
    assert!(
        build_chat_completion_request(&request)
            .expect_err("ambiguous token options")
            .message
            .contains("only one")
    );

    request.provider_options = Map::from_iter([("debug".into(), json!({}))]);
    assert!(
        build_chat_completion_request(&request)
            .expect_err("debug needs streaming")
            .message
            .contains("streaming-only")
    );

    request.provider_options = Map::from_iter([("models".into(), json!([GEMINI]))]);
    build_chat_completion_request(&request).expect("curated fallback accepted");
    request.provider_options = Map::from_iter([("models".into(), json!(["openrouter/free"]))]);
    assert_eq!(
        build_chat_completion_request(&request)
            .expect_err("uncurated fallback")
            .provider_type
            .as_deref(),
        Some("invalid_model")
    );
}

#[test]
fn native_tools_are_merged_with_portable_function_tools() {
    let mut request = request(GEMINI);
    request.tools.push(ToolDefinition::Function(FunctionTool {
        name: "weather".into(),
        description: "Get weather".into(),
        parameters: Map::from_iter([("type".into(), json!("object"))]),
        output_schema: None,
        strict: Some(true),
    }));
    request.provider_options.insert(
        "tools".into(),
        json!([{ "type": "native_search", "id": "future-tool" }]),
    );

    let body = build_chat_completion_request(&request).expect("valid tools");
    assert_eq!(body["tools"][0]["type"], "native_search");
    assert_eq!(body["tools"][1]["function"]["name"], "weather");
    assert_eq!(body["tools"][1]["function"]["strict"], true);
}

#[test]
fn native_message_and_reasoning_details_are_replayed_exactly() {
    let native_message = json!({
        "role": "assistant",
        "content": null,
        "reasoning": "Thought",
        "reasoning_details": [
            { "type": "reasoning.text", "text": "Thought", "signature": "opaque-a" },
            { "type": "reasoning.encrypted", "data": "opaque-b" }
        ],
        "tool_calls": [{
            "id": "call-1",
            "type": "function",
            "function": { "name": "weather", "arguments": "{\"city\":\"Paris\"}" }
        }]
    });
    let mut request = request(GEMINI);
    request.messages.push(Message::Assistant(AssistantMessage {
        id: MessageId::new("assistant-1").expect("valid id"),
        model: model_ref(GEMINI),
        usage: None,
        duration_ms: 1,
        native_message: json!({
            "id": "chatcmpl-previous",
            "object": "chat.completion",
            "choices": [{
                "index": 0,
                "message": native_message,
                "finish_reason": "tool_calls"
            }]
        }),
        content: vec![],
        stop_reason: StopReason::ToolUse,
        timestamp: Timestamp(1),
    }));

    let body = build_chat_completion_request(&request).expect("valid follow-up");
    assert_eq!(body["messages"][0], native_message);
}

#[test]
fn response_preserves_native_content_tools_usage_and_authoritative_cost() {
    let model = find_model(GEMINI).expect("catalog model");
    let native = json!({
        "id": "gen-1",
        "object": "chat.completion",
        "model": GEMINI,
        "choices": [{
            "index": 0,
            "message": {
                "role": "assistant",
                "reasoning": "Thought",
                "reasoning_details": [{ "type": "reasoning.text", "text": "Thought" }],
                "content": "Answer",
                "tool_calls": [{
                    "id": "call-1",
                    "type": "function",
                    "function": { "name": "weather", "arguments": "{\"city\":\"Paris\"}" }
                }]
            },
            "finish_reason": "tool_calls",
            "native_finish_reason": "tool_calls"
        }],
        "usage": {
            "prompt_tokens": 1_000,
            "completion_tokens": 100,
            "prompt_tokens_details": {
                "cached_tokens": 200,
                "cache_write_tokens": 100
            },
            "cost": 0.00123,
            "cost_details": {
                "upstream_inference_cost": 0.001,
                "upstream_inference_prompt_cost": 0.0004,
                "upstream_inference_completions_cost": 0.0006
            }
        },
        "provider": "Google",
        "future_field": { "preserved": true }
    });

    let message = convert_response(native.clone(), model, 123, 456).expect("valid response");
    assert_eq!(message.native_message, native);
    assert_eq!(message.model.id.as_str(), GEMINI);
    assert_eq!(message.model.name.as_deref(), Some(model.name));
    assert_eq!(message.stop_reason, StopReason::ToolUse);
    assert!(matches!(
        &message.content[0],
        AssistantContent::Thinking { thinking_text } if thinking_text == "Thought"
    ));
    assert!(matches!(
        &message.content[2],
        AssistantContent::ToolCall {
            arguments: ToolArguments::Object(arguments),
            tool_call_id,
            ..
        } if arguments["city"] == "Paris"
            && tool_call_id == &ToolCallId::new("call-1").expect("valid id")
    ));

    let usage = message.usage.expect("usage");
    assert_eq!(usage.input, Some(700));
    assert_eq!(usage.cache_read, Some(200));
    assert_eq!(usage.cache_write, Some(100));
    assert_eq!(usage.output, Some(100));
    let cost = usage.cost.expect("cost");
    assert_eq!(cost.input, Some(0.0004));
    assert_eq!(cost.output, Some(0.0006));
    assert_eq!(cost.cache_read, None);
    assert_eq!(cost.total, 0.00123);
}

#[test]
fn missing_native_cost_falls_back_to_catalog_pricing() {
    let model = find_model(GEMINI).expect("catalog model");
    let message = convert_response(
        json!({
            "id": "gen-no-cost",
            "object": "chat.completion",
            "model": GEMINI,
            "choices": [{
                "index": 0,
                "message": { "role": "assistant", "content": "Answer" },
                "finish_reason": "stop"
            }],
            "usage": {
                "prompt_tokens": 2_000_000,
                "completion_tokens": 1_000_000,
                "prompt_tokens_details": {
                    "cached_tokens": 1_000_000,
                    "cache_write_tokens": 500_000
                }
            }
        }),
        model,
        1,
        1,
    )
    .expect("valid response");
    let cost = message.usage.expect("usage").cost.expect("cost");
    let expected = 0.1875 + 1.875 + 0.0375 + 0.010_416_666_666_666_65;
    assert!((cost.total - expected).abs() < 1e-12);

    let helper = calculate_usage_cost(
        &llm_contracts::Usage {
            input: Some(1_000_000),
            output: Some(1_000_000),
            cache_read: Some(1_000_000),
            cache_write: Some(1_000_000),
            cost: None,
        },
        model,
    );
    assert!((helper.total - 2.308_333_333_333_333).abs() < 1e-12);
}

#[test]
fn choice_level_errors_are_normalized_even_on_http_success() {
    let model = find_model(GEMINI).expect("catalog model");
    let native = json!({
        "id": "gen-error",
        "object": "chat.completion",
        "model": GEMINI,
        "choices": [{
            "index": 0,
            "finish_reason": "error",
            "error": {
                "code": 502,
                "message": "upstream failed",
                "metadata": {
                    "error_type": "provider_error",
                    "provider_code": "upstream-502"
                }
            }
        }]
    });
    let error = convert_response(native.clone(), model, 1, 1).expect_err("choice error must fail");
    assert_eq!(error.message, "upstream failed");
    assert_eq!(error.http_status, Some(200));
    assert_eq!(error.provider_type.as_deref(), Some("provider_error"));
    assert_eq!(error.provider_code.as_deref(), Some("upstream-502"));
    assert_eq!(error.native_error.as_deref(), Some(&native));
}
