use std::collections::BTreeMap;

use llm_contracts::{
    AssistantContent, AssistantMessage, ContentPart, CustomTool, CustomToolFormat, FunctionTool,
    GrammarSyntax, ImageContent, ImageDetail, ImageSource, LlmRequest, Message, MessageId, ModelId,
    ModelRef, ProviderId, StopReason, TextContent, Timestamp, ToolArguments, ToolCallId,
    ToolDefinition, UserMessage,
};
use provider_fireworks::{
    FIREWORKS_MODELS, build_chat_completion_request, calculate_usage_cost, convert_response,
    find_model, reasoning_effort,
};
use serde_json::{Map, json};

fn model_ref(id: &str) -> ModelRef {
    ModelRef {
        provider: ProviderId::new("fireworks").expect("valid provider id"),
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
    assert_eq!(FIREWORKS_MODELS.len(), 4);
    assert!(find_model("accounts/fireworks/models/kimi-k3").is_some());
    assert!(find_model("accounts/fireworks/models/deepseek-v4-pro-0813").is_some());
    assert!(find_model("accounts/fireworks/models/qwen3p8-2p4t-a95b").is_some());
    assert!(find_model("accounts/fireworks/models/deepseek-v4-flash-0731").is_some());
    assert!(find_model("accounts/fireworks/models/glm-5p2").is_none());
    assert!(find_model("accounts/fireworks/models/deepseek-v4-flash").is_none());

    let error = build_chat_completion_request(&request("accounts/fireworks/models/unknown"))
        .expect_err("unknown model must be rejected");
    assert_eq!(error.provider_type.as_deref(), Some("invalid_model"));
}

#[test]
fn maps_portable_reasoning_to_each_models_native_efforts() {
    let levels = ["low", "medium", "high", "xhigh", "max"];
    let expected = [
        (
            "accounts/fireworks/models/kimi-k3",
            ["low", "medium", "high", "max", "max"],
        ),
        (
            "accounts/fireworks/models/deepseek-v4-pro-0813",
            ["high", "high", "high", "max", "max"],
        ),
        (
            "accounts/fireworks/models/qwen3p8-2p4t-a95b",
            ["low", "medium", "high", "high", "high"],
        ),
        (
            "accounts/fireworks/models/deepseek-v4-flash-0731",
            ["low", "high", "high", "max", "max"],
        ),
    ];

    assert_eq!(expected.len(), FIREWORKS_MODELS.len());
    for (model_id, efforts) in expected {
        assert!(find_model(model_id).is_some());
        for (level, effort) in levels.into_iter().zip(efforts) {
            assert_eq!(reasoning_effort(model_id, level), Some(effort));
        }
    }

    assert_eq!(reasoning_effort("unknown", "high"), None);
    assert_eq!(
        reasoning_effort("accounts/fireworks/models/kimi-k3", "minimal"),
        None
    );
}

#[test]
fn request_is_non_streaming_and_catalog_owned_fields_win() {
    let mut request = request("accounts/fireworks/models/kimi-k3");
    request.instructions = Some("Be concise".into());
    request.messages.push(Message::User(UserMessage {
        id: MessageId::new("user-1").expect("valid id"),
        timestamp: Timestamp(1),
        content: vec![
            ContentPart::Text(TextContent {
                content: "Describe this".into(),
                metadata: None,
            }),
            ContentPart::Image(ImageContent {
                source: ImageSource::Url(llm_contracts::UrlImageSource {
                    url: "https://example.com/image.png".into(),
                }),
                detail: Some(ImageDetail::Original),
                metadata: None,
            }),
        ],
    }));
    request.provider_options = Map::from_iter([
        ("model".into(), json!("bypass")),
        ("messages".into(), json!([])),
        ("stream".into(), json!(true)),
        ("n".into(), json!(8)),
        ("max_tokens".into(), json!(1_000_000)),
        ("reasoning_effort".into(), json!("max")),
        ("prompt_cache_key".into(), json!("session-123")),
    ]);

    let body = build_chat_completion_request(&request).expect("valid request");

    assert_eq!(body["model"], "accounts/fireworks/models/kimi-k3");
    assert_eq!(body["stream"], false);
    assert_eq!(body["n"], 1);
    assert_eq!(body["max_tokens"], 1_000_000);
    assert_eq!(body["reasoning_effort"], "max");
    assert_eq!(body["prompt_cache_key"], "session-123");
    assert_eq!(body["messages"][0]["role"], "system");
    assert_eq!(body["messages"][1]["content"][0]["type"], "text");
    assert_eq!(
        body["messages"][1]["content"][1]["image_url"]["detail"],
        "high"
    );
}

#[test]
fn prompt_token_ids_replace_messages() {
    let mut request = request("accounts/fireworks/models/qwen3p8-2p4t-a95b");
    request.instructions = Some("not sent".into());
    request
        .provider_options
        .insert("prompt_token_ids".into(), json!([1, 2, 3]));

    let body = build_chat_completion_request(&request).expect("valid request");
    assert!(body.get("messages").is_none());
    assert_eq!(body["prompt_token_ids"], json!([1, 2, 3]));
}

#[test]
fn request_maps_function_tools_and_rejects_unpriced_or_unsupported_modes() {
    let mut request = request("accounts/fireworks/models/deepseek-v4-pro-0813");
    request.tools.push(ToolDefinition::Function(FunctionTool {
        name: "weather".into(),
        description: "Get weather".into(),
        parameters: Map::from_iter([("type".into(), json!("object"))]),
        output_schema: None,
        strict: Some(true),
    }));

    let body = build_chat_completion_request(&request).expect("function tool is supported");
    assert_eq!(body["tools"][0]["type"], "function");
    assert_eq!(body["tools"][0]["function"]["strict"], true);

    request
        .provider_options
        .insert("service_tier".into(), json!("priority"));
    let error = build_chat_completion_request(&request).expect_err("priority pricing differs");
    assert!(error.message.contains("standard-tier pricing"));

    request.provider_options.clear();
    request.tools = vec![ToolDefinition::Custom(CustomTool {
        name: "grammar".into(),
        description: "Constrained output".into(),
        format: CustomToolFormat {
            syntax: GrammarSyntax::Lark,
            definition: "start: WORD".into(),
        },
    })];
    let error = build_chat_completion_request(&request).expect_err("custom tools unsupported");
    assert!(
        error
            .message
            .contains("does not support portable custom tool")
    );
}

#[test]
fn native_choice_zero_is_replayed_on_follow_up_requests() {
    let native_message = json!({
        "role": "assistant",
        "content": "Answer",
        "reasoning_content": "Thought",
        "internal_content": { "opaque": true }
    });
    let mut request = request("accounts/fireworks/models/kimi-k3");
    request.messages.push(Message::Assistant(AssistantMessage {
        id: MessageId::new("assistant-1").expect("valid id"),
        model: model_ref("accounts/fireworks/models/kimi-k3"),
        usage: None,
        duration_ms: 1,
        native_message: json!({
            "id": "chatcmpl-previous",
            "object": "chat.completion",
            "choices": [{
                "index": 0,
                "message": native_message,
                "finish_reason": "stop"
            }]
        }),
        content: vec![AssistantContent::Response {
            response: TextContent {
                content: "normalized".into(),
                metadata: None,
            },
        }],
        stop_reason: StopReason::Stop,
        timestamp: Timestamp(1),
    }));

    let body = build_chat_completion_request(&request).expect("valid follow-up");
    assert_eq!(body["messages"][0], native_message);
}

#[test]
fn response_preserves_native_content_tools_usage_and_standard_cost() {
    let model = find_model("accounts/fireworks/models/kimi-k3").expect("catalog model");
    let native = json!({
        "id": "chatcmpl-1",
        "object": "chat.completion",
        "created": 1,
        "model": "accounts/fireworks/models/kimi-k3",
        "choices": [{
            "index": 0,
            "message": {
                "role": "assistant",
                "reasoning_content": "Thought",
                "content": "Answer",
                "internal_content": { "opaque": true },
                "tool_calls": [{
                    "id": "call-1",
                    "type": "function",
                    "function": { "name": "weather", "arguments": "{\"city\":\"Paris\"}" }
                }]
            },
            "finish_reason": "tool_calls"
        }],
        "usage": {
            "prompt_tokens": 1_000_000,
            "completion_tokens": 1_000_000,
            "total_tokens": 2_000_000,
            "prompt_tokens_details": { "cached_tokens": 100_000 }
        },
        "future_field": { "preserved": true }
    });

    let message = convert_response(native.clone(), model, 123, 456).expect("valid response");

    assert_eq!(message.native_message, native);
    assert_eq!(message.model.id.as_str(), model.id);
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
    assert_eq!(usage.input, Some(900_000));
    assert_eq!(usage.cache_read, Some(100_000));
    assert_eq!(usage.output, Some(1_000_000));
    let cost = usage.cost.expect("cost");
    assert!((cost.input.expect("input") - 2.7).abs() < f64::EPSILON);
    assert!((cost.cache_read.expect("cache") - 0.03).abs() < f64::EPSILON);
    assert!((cost.output.expect("output") - 15.0).abs() < f64::EPSILON);
    assert!((cost.total - 17.73).abs() < f64::EPSILON);
    assert_eq!(cost.cache_write, None);
}

#[test]
fn malformed_tool_arguments_are_retained_and_stop_reasons_are_mapped() {
    let model = find_model("accounts/fireworks/models/qwen3p8-2p4t-a95b").expect("catalog model");
    let message = convert_response(
        json!({
            "id": "chatcmpl-tools",
            "object": "chat.completion",
            "model": model.id,
            "choices": [{
                "index": 0,
                "message": {
                    "role": "assistant",
                    "content": null,
                    "tool_calls": [{
                        "id": "call-1",
                        "type": "function",
                        "function": { "name": "tool", "arguments": "[1]" }
                    }]
                },
                "finish_reason": "function_call"
            }]
        }),
        model,
        1,
        1,
    )
    .expect("valid response");
    assert_eq!(message.stop_reason, StopReason::ToolUse);
    assert!(matches!(
        &message.content[0],
        AssistantContent::ToolCall { arguments: ToolArguments::String(value), .. }
            if value == "[1]"
    ));

    for (native_reason, expected) in [
        ("stop", StopReason::Stop),
        ("length", StopReason::Length),
        ("content_filter", StopReason::ContentFilter),
    ] {
        let converted = convert_response(
            json!({
                "id": format!("chatcmpl-{native_reason}"),
                "object": "chat.completion",
                "model": model.id,
                "choices": [{
                    "index": 0,
                    "message": { "role": "assistant", "content": "result" },
                    "finish_reason": native_reason
                }]
            }),
            model,
            1,
            1,
        )
        .expect("known stop reason");
        assert_eq!(converted.stop_reason, expected);
    }
}

#[test]
fn cost_helper_prices_uncached_cached_and_output_tokens() {
    let model =
        find_model("accounts/fireworks/models/deepseek-v4-flash-0731").expect("catalog model");
    let cost = calculate_usage_cost(
        &llm_contracts::Usage {
            input: Some(1_000_000),
            output: Some(1_000_000),
            cache_read: Some(1_000_000),
            cache_write: None,
            cost: None,
        },
        model,
    );
    assert!((cost.total - 0.448).abs() < f64::EPSILON);
}
