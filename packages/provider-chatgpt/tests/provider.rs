use std::collections::BTreeMap;

use llm_contracts::{
    AssistantContent, LlmRequest, Message, ModelId, ModelRef, ProviderId, StopReason,
};
use provider_chatgpt::{
    CHATGPT_MODELS, build_response_request, convert_response_events, find_model,
};
use serde_json::{Map, json};

fn model_ref(id: &str) -> ModelRef {
    ModelRef {
        provider: ProviderId::new("chatgpt").expect("valid provider id"),
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
fn catalog_is_the_openai_catalog_and_a_strict_allowlist() {
    assert_eq!(CHATGPT_MODELS, provider_openai::OPENAI_MODELS);
    assert_eq!(CHATGPT_MODELS.len(), 3);
    assert!(find_model("gpt-5.6-sol").is_some());

    let error =
        build_response_request(&request("unknown")).expect_err("unknown model must be rejected");
    assert_eq!(error.provider_type.as_deref(), Some("invalid_model"));
}

#[test]
fn request_forces_chatgpt_backend_fields() {
    let mut request = request("gpt-5.6-terra");
    request.provider_options = Map::from_iter([
        ("model".into(), json!("bypass")),
        ("store".into(), json!(true)),
        ("stream".into(), json!(false)),
        ("include".into(), json!(["something_else"])),
        ("temperature".into(), json!(0.2)),
    ]);

    let body = build_response_request(&request).expect("valid request");

    assert_eq!(body["model"], "gpt-5.6-terra");
    assert_eq!(body["store"], false);
    assert_eq!(body["stream"], true);
    assert_eq!(body["include"], json!(["reasoning.encrypted_content"]));
    assert_eq!(body["instructions"], "You are a helpful assistant.");
    assert_eq!(body["temperature"], 0.2);
}

#[test]
fn request_rejects_the_api_only_max_output_tokens_option() {
    let mut request = request("gpt-5.6-terra");
    request
        .provider_options
        .insert("max_output_tokens".into(), json!(64));

    let error = build_response_request(&request).expect_err("option must be rejected locally");
    assert_eq!(error.provider_type.as_deref(), Some("invalid_request"));
    assert!(error.message.contains("does not support"));
}

#[test]
fn response_aggregates_native_items_and_preserves_every_event() {
    let model = find_model("gpt-5.6-luna").expect("catalog model");
    let events = vec![
        json!({
            "type": "response.created",
            "response": { "id": "response-1", "model": "resolved-model" }
        }),
        json!({
            "type": "response.output_item.done",
            "item": {
                "type": "reasoning",
                "id": "reasoning-1",
                "encrypted_content": "opaque",
                "summary": [{ "type": "summary_text", "text": "Thought" }]
            }
        }),
        json!({
            "type": "response.output_item.done",
            "item": {
                "type": "message",
                "id": "message-1",
                "role": "assistant",
                "content": [{ "type": "output_text", "text": "Answer" }]
            }
        }),
        json!({
            "type": "response.completed",
            "response": {
                "id": "response-1",
                "model": "resolved-model",
                "status": "completed",
                "output": [],
                "usage": {
                    "input_tokens": 10,
                    "output_tokens": 2,
                    "input_tokens_details": { "cached_tokens": 3 }
                }
            }
        }),
    ];

    let message = convert_response_events(events.clone(), model, 12, 34).expect("valid response");

    assert_eq!(message.id.as_str(), "response-1");
    assert_eq!(message.model.provider.as_str(), "chatgpt");
    assert_eq!(message.model.id.as_str(), "gpt-5.6-luna");
    assert_eq!(message.stop_reason, StopReason::Stop);
    assert_eq!(message.native_message["events"], json!(events));
    assert_eq!(
        message.native_message["output"].as_array().map(Vec::len),
        Some(2)
    );
    assert!(matches!(
        &message.content[0],
        AssistantContent::Thinking { thinking_text } if thinking_text == "Thought"
    ));
    assert!(message.usage.expect("usage").cost.is_some());
}

#[test]
fn native_output_is_replayed_on_follow_up_requests() {
    let model = find_model("gpt-5.6-luna").expect("catalog model");
    let native_item = json!({
        "type": "reasoning",
        "id": "reasoning-1",
        "encrypted_content": "opaque"
    });
    let assistant = convert_response_events(
        vec![
            json!({ "type": "response.output_item.done", "item": native_item }),
            json!({
                "type": "response.completed",
                "response": {
                    "id": "response-1",
                    "model": "gpt-5.6-luna",
                    "status": "completed"
                }
            }),
        ],
        model,
        1,
        1,
    )
    .expect("valid response");
    let expected = assistant.native_message["output"].clone();
    let mut request = request("gpt-5.6-luna");
    request.messages.push(Message::Assistant(assistant));

    let body = build_response_request(&request).expect("valid follow-up request");
    assert_eq!(body["input"], expected);
}

#[test]
fn provider_failure_retains_the_native_event() {
    let model = find_model("gpt-5.6-luna").expect("catalog model");
    let failed = json!({
        "type": "response.failed",
        "response": {
            "error": {
                "message": "Rejected",
                "type": "provider_error",
                "code": "rejected"
            }
        }
    });
    let error = convert_response_events(vec![failed.clone()], model, 1, 1)
        .expect_err("failed response must return an error");

    assert_eq!(error.message, "Rejected");
    assert_eq!(error.provider_code.as_deref(), Some("rejected"));
    assert_eq!(error.provider_type.as_deref(), Some("provider_error"));
    assert_eq!(error.native_error, Some(Box::new(failed)));
}
