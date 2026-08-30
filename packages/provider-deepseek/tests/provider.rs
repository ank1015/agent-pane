use std::collections::BTreeMap;

use llm_contracts::{
    AssistantContent, AssistantMessage, ContentPart, FunctionTool, ImageContent, ImageDetail,
    ImageSource, LlmRequest, Message, MessageId, ModelId, ModelRef, ProviderId, StopReason,
    TextContent, Timestamp, ToolArguments, ToolCallId, ToolDefinition, UrlImageSource, UserMessage,
};
use provider_deepseek::{
    DEEPSEEK_MODELS, build_chat_completion_request, calculate_usage_cost, convert_response,
    find_model, is_peak_pricing, reasoning_effort,
};
use serde_json::{Map, json};

const FLASH: &str = "deepseek-v4-flash";
const PRO: &str = "deepseek-v4-pro";
const VISION: &str = "deepseek-v4-flash-vision-exp";

fn model_ref(id: &str) -> ModelRef {
    ModelRef {
        provider: ProviderId::new("deepseek").expect("valid provider id"),
        id: ModelId::new(id).expect("valid model id"),
        name: None,
    }
}

fn request(model_id: &str) -> LlmRequest {
    LlmRequest {
        model: model_ref(model_id),
        instructions: None,
        messages: vec![Message::User(UserMessage {
            id: MessageId::new("user-1").expect("valid id"),
            timestamp: Timestamp(1),
            content: vec![ContentPart::Text(TextContent {
                content: "Hello".into(),
                metadata: None,
            })],
        })],
        tools: Vec::new(),
        provider_options: Map::new(),
        metadata: BTreeMap::new(),
    }
}

#[test]
fn catalog_is_exact_and_uses_announced_limits_and_pricing() {
    assert_eq!(DEEPSEEK_MODELS.len(), 3);
    let flash = find_model(FLASH).expect("flash model");
    assert_eq!(flash.name, "DeepSeek V4 Flash 0731");
    assert_eq!(flash.context_window, 1_048_576);
    assert_eq!(flash.max_tokens, 384_000);
    assert_eq!(flash.pricing.base.input, 0.22);
    assert_eq!(flash.pricing.base.output, 0.66);
    assert_eq!(flash.peak_pricing.input, 0.44);
    assert!(!flash.supports_images);

    let pro = find_model(PRO).expect("pro model");
    assert_eq!(pro.name, "DeepSeek V4 Pro 0813");
    assert_eq!(pro.context_window, 1_048_576);
    assert_eq!(pro.max_tokens, 384_000);
    assert_eq!(pro.pricing.base.input, 0.66);
    assert_eq!(pro.pricing.base.output, 1.98);
    assert_eq!(pro.peak_pricing.input, 1.32);
    assert!(!pro.supports_images);

    let vision = find_model(VISION).expect("vision model");
    assert_eq!(vision.pricing.base.input, 0.22);
    assert_eq!(vision.pricing.base.cache_read, 0.007);
    assert_eq!(vision.peak_pricing.output, 1.32);
    assert!(vision.supports_images);
    assert!(find_model("deepseek-v4-flash-0731").is_none());

    let error = build_chat_completion_request(&request("deepseek-v4-flash-0731"))
        .expect_err("uncurated model must fail");
    assert_eq!(error.provider_type.as_deref(), Some("invalid_model"));
}

#[test]
fn maps_every_portable_reasoning_level_for_every_model() {
    let levels = ["low", "medium", "high", "xhigh", "max"];
    let expected = ["low", "high", "high", "high", "max"];

    for model in DEEPSEEK_MODELS {
        for (level, effort) in levels.into_iter().zip(expected) {
            assert_eq!(reasoning_effort(model.id, level), Some(effort));
        }
    }

    assert_eq!(reasoning_effort("unknown", "high"), None);
    assert_eq!(reasoning_effort(PRO, "minimal"), None);
}

#[test]
fn request_forces_non_streaming_and_validates_max_tokens() {
    let mut request = request(PRO);
    request.instructions = Some("Be concise".into());
    request.provider_options = Map::from_iter([
        ("model".into(), json!("bypass")),
        ("messages".into(), json!([])),
        ("stream".into(), json!(true)),
        ("stream_options".into(), json!({ "include_usage": true })),
        ("n".into(), json!(4)),
        ("max_tokens".into(), json!(384_000)),
        ("thinking".into(), json!({ "type": "enabled" })),
        ("reasoning_effort".into(), json!("max")),
    ]);

    let body = build_chat_completion_request(&request).expect("valid request");
    assert_eq!(body["model"], PRO);
    assert_eq!(body["stream"], false);
    assert!(body.get("n").is_none());
    assert!(body.get("stream_options").is_none());
    assert_eq!(body["max_tokens"], 384_000);
    assert_eq!(body["thinking"]["type"], "enabled");
    assert_eq!(body["messages"][0]["role"], "system");

    request
        .provider_options
        .insert("max_tokens".into(), json!(384_001));
    assert!(
        build_chat_completion_request(&request)
            .expect_err("above output limit")
            .message
            .contains("cannot exceed 384000")
    );
}

#[test]
fn only_the_vision_model_accepts_images_and_preserves_original_detail() {
    let image = ContentPart::Image(ImageContent {
        source: ImageSource::Url(UrlImageSource {
            url: "https://example.com/image.png".into(),
        }),
        detail: Some(ImageDetail::Original),
        metadata: None,
    });
    let mut pro = request(PRO);
    if let Message::User(message) = &mut pro.messages[0] {
        message.content.push(image.clone());
    }
    assert!(
        build_chat_completion_request(&pro)
            .expect_err("pro is text-only")
            .message
            .contains("does not support image")
    );

    let mut vision = request(VISION);
    if let Message::User(message) = &mut vision.messages[0] {
        message.content.push(image);
    }
    let body = build_chat_completion_request(&vision).expect("vision request");
    assert_eq!(body["messages"][0]["content"][1]["type"], "image_url");
    assert_eq!(
        body["messages"][0]["content"][1]["image_url"]["detail"],
        "original"
    );
}

#[test]
fn native_assistant_message_is_replayed_without_losing_reasoning() {
    let native_message = json!({
        "role": "assistant",
        "content": null,
        "reasoning_content": "Thought",
        "tool_calls": [{
            "id": "call-1",
            "type": "function",
            "function": { "name": "weather", "arguments": "{\"city\":\"Paris\"}" }
        }],
        "future": { "opaque": true }
    });
    let mut request = request(PRO);
    request.messages = vec![Message::Assistant(AssistantMessage {
        id: MessageId::new("assistant-1").expect("valid id"),
        model: model_ref(PRO),
        usage: None,
        duration_ms: 1,
        native_message: json!({
            "id": "previous",
            "object": "chat.completion",
            "model": PRO,
            "choices": [{
                "index": 0,
                "message": native_message,
                "finish_reason": "tool_calls"
            }]
        }),
        content: Vec::new(),
        stop_reason: StopReason::ToolUse,
        timestamp: Timestamp(1),
    })];

    let body = build_chat_completion_request(&request).expect("valid follow-up");
    assert_eq!(body["messages"][0], native_message);
}

#[test]
fn tool_requests_add_missing_reasoning_content_to_assistant_history() {
    let mut request = request(PRO);
    request.tools.push(ToolDefinition::Function(FunctionTool {
        name: "weather".into(),
        description: "Get weather".into(),
        parameters: Map::from_iter([("type".into(), json!("object"))]),
        output_schema: None,
        strict: None,
    }));
    request.messages = vec![Message::Assistant(AssistantMessage {
        id: MessageId::new("assistant-1").expect("valid id"),
        model: ModelRef {
            provider: ProviderId::new("openai").expect("valid provider"),
            id: ModelId::new("gpt-5.6-terra").expect("valid model"),
            name: None,
        },
        usage: None,
        duration_ms: 1,
        native_message: json!({}),
        content: vec![AssistantContent::Response {
            response: TextContent {
                content: "Previous answer".into(),
                metadata: None,
            },
        }],
        stop_reason: StopReason::Stop,
        timestamp: Timestamp(1),
    })];
    request
        .provider_options
        .insert("thinking".into(), json!({"type": "enabled"}));

    let body = build_chat_completion_request(&request).expect("thinking tool request");
    assert_eq!(body["messages"][0]["reasoning_content"], "");

    request
        .provider_options
        .insert("thinking".into(), json!({"type": "disabled"}));
    let body = build_chat_completion_request(&request).expect("non-thinking tool request");
    assert!(body["messages"][0].get("reasoning_content").is_none());
}

#[test]
fn maps_function_tools_and_response_content_usage_and_cost() {
    let mut request = request(PRO);
    request.tools.push(ToolDefinition::Function(FunctionTool {
        name: "weather".into(),
        description: "Get weather".into(),
        parameters: Map::from_iter([("type".into(), json!("object"))]),
        output_schema: None,
        strict: Some(true),
    }));
    let body = build_chat_completion_request(&request).expect("valid tool");
    assert_eq!(body["tools"][0]["function"]["strict"], true);

    let model = find_model(PRO).expect("model");
    let native = json!({
        "id": "completion-1",
        "object": "chat.completion",
        "model": PRO,
        "choices": [{
            "index": 0,
            "message": {
                "role": "assistant",
                "reasoning_content": "Thought",
                "content": "Answer",
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
            "prompt_cache_hit_tokens": 100_000,
            "prompt_cache_miss_tokens": 900_000
        },
        "system_fingerprint": "opaque"
    });
    let message = convert_response(native.clone(), model, 0, 0).expect("valid response");
    assert_eq!(message.native_message, native);
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
    let cost = usage.cost.expect("cost");
    assert!((cost.input.expect("input") - 0.594).abs() < 1e-12);
    assert!((cost.cache_read.expect("cache") - 0.0022).abs() < 1e-12);
    assert!((cost.output.expect("output") - 1.98).abs() < 1e-12);
    assert!((cost.total - 2.5762).abs() < 1e-12);
}

#[test]
fn pricing_windows_and_both_tiers_are_exact() {
    let day = 24 * 60 * 60 * 1_000;
    let hour = 60 * 60 * 1_000;
    assert!(!is_peak_pricing(0));
    assert!(is_peak_pricing(hour));
    assert!(is_peak_pricing(3 * hour + 59 * 60 * 1_000));
    assert!(!is_peak_pricing(4 * hour));
    assert!(is_peak_pricing(6 * hour));
    assert!(!is_peak_pricing(10 * hour));
    assert!(is_peak_pricing(4 * day + hour)); // Monday
    assert!(!is_peak_pricing(9 * day + hour)); // Saturday
    assert!(!is_peak_pricing(10 * day + hour)); // Sunday

    let usage = llm_contracts::Usage {
        input: Some(1_000_000),
        output: Some(1_000_000),
        cache_read: Some(1_000_000),
        cache_write: None,
        cost: None,
    };
    let model = find_model(VISION).expect("vision model");
    assert!((calculate_usage_cost(&usage, model, 0).total - 0.887).abs() < 1e-12);
    assert!((calculate_usage_cost(&usage, model, hour).total - 1.774).abs() < 1e-12);
}

#[test]
fn insufficient_resources_are_a_retryable_provider_failure() {
    let model = find_model(PRO).expect("model");
    let native = json!({
        "id": "interrupted",
        "object": "chat.completion",
        "model": PRO,
        "choices": [{
            "index": 0,
            "message": { "role": "assistant", "content": null },
            "finish_reason": "insufficient_system_resource"
        }]
    });
    let error = convert_response(native.clone(), model, 1, 1).expect_err("must fail");
    assert_eq!(
        error.provider_type.as_deref(),
        Some("insufficient_system_resource")
    );
    assert_eq!(error.http_status, Some(200));
    assert!(error.can_retry);
    assert_eq!(error.native_error.as_deref(), Some(&native));
}
