use llm_contracts::{
    AssistantMessage, FunctionTool, LlmRequest, Message, Validate, validation::from_json_value,
};
use schemars::JsonSchema;
use serde::Deserialize;
use serde_json::json;

#[test]
fn parses_snake_case_user_message() {
    let message: Message = from_json_value(json!({
        "role": "user",
        "id": "message-1",
        "timestamp": 1_700_000_000_000_u64,
        "content": [{
            "type": "text",
            "content": "Hello"
        }]
    }))
    .expect("valid user message");

    let serialized = serde_json::to_value(message).expect("serializable message");
    assert_eq!(serialized["role"], "user");
    assert_eq!(serialized["content"][0]["type"], "text");
}

#[test]
fn rejects_unknown_message_fields() {
    let result = from_json_value::<Message>(json!({
        "role": "system",
        "id": "message-1",
        "timestamp": 1,
        "content": [{ "content": "Be concise" }],
        "unexpected": true
    }));

    assert!(result.is_err());
}

#[test]
fn native_assistant_message_is_required_and_round_trips() {
    let native_message = json!({
        "id": "provider-message-1",
        "provider_only_field": { "value": 42 }
    });
    let value = json!({
        "id": "message-2",
        "model": {
            "provider": "example_provider",
            "id": "example-model"
        },
        "duration_ms": 125,
        "native_message": native_message,
        "content": [{
            "type": "response",
            "response": { "content": "Hello" }
        }],
        "stop_reason": "stop",
        "timestamp": 1_700_000_000_001_u64
    });

    let message: AssistantMessage =
        from_json_value(value.clone()).expect("valid assistant message");
    assert_eq!(message.native_message, native_message);
    assert_eq!(
        serde_json::to_value(message).expect("serializable assistant message"),
        value
    );

    let mut missing_native = value;
    missing_native
        .as_object_mut()
        .expect("assistant message is an object")
        .remove("native_message");
    assert!(from_json_value::<AssistantMessage>(missing_native).is_err());
}

#[test]
fn request_rejects_duplicate_tool_names() {
    let tool = json!({
        "type": "function",
        "name": "read_file",
        "description": "Read a file",
        "parameters": {
            "type": "object",
            "properties": {
                "path": { "type": "string" }
            },
            "required": ["path"]
        }
    });
    let request = json!({
        "model": {
            "provider": "example_provider",
            "id": "example-model"
        },
        "messages": [],
        "tools": [tool.clone(), tool]
    });

    let error = from_json_value::<LlmRequest>(request).expect_err("duplicate tools must fail");
    assert!(error.to_string().contains("duplicated"));
}

#[test]
fn request_rejects_invalid_image_url() {
    let request: LlmRequest = serde_json::from_value(json!({
        "model": {
            "provider": "example_provider",
            "id": "example-model"
        },
        "messages": [{
            "role": "user",
            "id": "message-1",
            "timestamp": 1,
            "content": [{
                "type": "image",
                "source": {
                    "type": "url",
                    "url": "not-a-url"
                }
            }]
        }]
    }))
    .expect("structurally valid request");

    assert!(request.validate().is_err());
}

#[derive(Deserialize, JsonSchema)]
#[allow(dead_code)]
struct WeatherArguments {
    city: String,
}

#[test]
fn generates_function_parameters_from_a_rust_type() {
    let tool = FunctionTool::for_type::<WeatherArguments>(
        "get_weather",
        "Get the current weather for a city",
    )
    .expect("valid tool");

    assert_eq!(tool.parameters["type"], "object");
    assert_eq!(tool.parameters["properties"]["city"]["type"], "string");
    assert_eq!(tool.parameters["required"], json!(["city"]));
}

#[test]
fn generates_a_schema_for_the_complete_request() {
    let schema = schemars::schema_for!(LlmRequest);
    let value = serde_json::to_value(schema).expect("serializable schema");

    assert_eq!(value["type"], "object");
    assert!(value["properties"]["model"].is_object());
    assert!(value["properties"]["messages"].is_object());
}
