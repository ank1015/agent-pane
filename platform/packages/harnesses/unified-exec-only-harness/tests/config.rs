use llm_contracts::*;
use serde_json::{Value, json};
use unified_exec_only_harness::{Config, config_schema, supported_models};
use uuid::Uuid;

fn config(provider: &str, model: &str, effort: &str) -> Value {
    json!({"model":{"provider":provider,"id":model},"reasoning_level":effort,
        "environment":{"type":"machine","machine_id":Uuid::new_v4(),"workspace_root":"/work","path":"."}})
}

#[test]
fn astra_is_available_through_both_supported_providers() {
    let models = supported_models();
    for provider in ["openai", "chatgpt"] {
        assert!(models[provider].iter().any(|id| id == "gpt-6-astra"));
    }
}

#[test]
fn provider_policies_preserve_native_history_and_accept_two_function_tools() {
    for (provider, models) in supported_models() {
        for model in models {
            for effort in ["low", "medium", "high", "xhigh", "max"] {
                let config =
                    Config::parse(config(&provider, &model, effort).as_object().unwrap()).unwrap();
                let session = Uuid::new_v4();
                let options = config.provider_options(session).unwrap();
                let native = if provider == "fireworks" {
                    assert_eq!(options, json!({"reasoning_effort":provider_fireworks::reasoning_effort(&model,effort).unwrap(),
                        "prompt_cache_key":session.to_string(),"max_tokens":provider_fireworks::find_model(&model).unwrap().max_tokens}).as_object().unwrap().clone());
                    json!({"choices":[{"index":0,"message":{"role":"assistant","content":null,"reasoning_content":"preserved reasoning",
                        "tool_calls":[{"id":"call_1","type":"function","function":{"name":"exec_command","arguments":"{\"cmd\":\"pwd\"}"}}]}}]})
                } else {
                    assert_eq!(options, json!({"store":false,"include":["reasoning.encrypted_content"],"prompt_cache_key":session.to_string(),
                        "tool_choice":"auto","parallel_tool_calls":false,"reasoning":{"effort":effort},"text":{"verbosity":"low"}}).as_object().unwrap().clone());
                    json!({"output":[{"type":"reasoning","id":"rs_1","encrypted_content":"opaque","summary":[]},
                        {"type":"function_call","id":"fc_1","call_id":"call_1","name":"exec_command","arguments":"{\"cmd\":\"pwd\"}"}]})
                };
                let request = LlmRequest {
                    model: config.model.clone(),
                    instructions: Some("Use shell commands for all file work".into()),
                    tools: tool_unified_exec::definitions(),
                    messages: vec![
                        Message::Assistant(AssistantMessage {
                            id: MessageId::new("assistant").unwrap(),
                            model: config.model.clone(),
                            content: vec![],
                            native_message: native.clone(),
                            usage: None,
                            duration_ms: 0,
                            stop_reason: StopReason::ToolUse,
                            timestamp: Timestamp(0),
                        }),
                        Message::ToolResult(ToolResultMessage {
                            id: MessageId::new("result").unwrap(),
                            tool_name: "exec_command".into(),
                            tool_call_id: ToolCallId::new("call_1").unwrap(),
                            content: vec![ContentPart::Text(TextContent {
                                content: "/work".into(),
                                metadata: None,
                            })],
                            details: None,
                            outcome: ToolResultOutcome::Success,
                            timestamp: Timestamp(0),
                        }),
                    ],
                    provider_options: options,
                    metadata: Default::default(),
                };
                let body = match provider.as_str() {
                    "openai" => provider_openai::build_response_request(&request),
                    "chatgpt" => provider_chatgpt::build_response_request(&request),
                    "fireworks" => provider_fireworks::build_chat_completion_request(&request),
                    _ => unreachable!(),
                }
                .unwrap();
                assert_eq!(body["tools"].as_array().unwrap().len(), 2);
                assert!(
                    body["tools"]
                        .as_array()
                        .unwrap()
                        .iter()
                        .all(|t| t["type"] == "function")
                );
                if provider == "fireworks" {
                    assert_eq!(body["messages"][1], native["choices"][0]["message"]);
                    assert_eq!(
                        body["messages"][2],
                        json!({"role":"tool","tool_call_id":"call_1","content":"/work"})
                    );
                } else {
                    assert_eq!(
                        &body["input"].as_array().unwrap()[..2],
                        native["output"].as_array().unwrap()
                    );
                    assert_eq!(
                        body["input"][2],
                        json!({"type":"function_call_output","call_id":"call_1","output":"/work"})
                    );
                    assert!(body.get("max_output_tokens").is_none());
                    assert!(body.get("codex_responses_lite").is_none());
                }
            }
        }
    }
}

#[test]
fn unknown_models_levels_and_raw_provider_overrides_are_rejected() {
    for invalid in [
        config("unknown", "gpt-5.6-terra", "high"),
        config("openai", "unknown", "high"),
        config("fireworks", "unknown", "high"),
        config("chatgpt", "gpt-5.6-terra", "ultra"),
    ] {
        assert!(Config::parse(invalid.as_object().unwrap()).is_err());
    }
    let mut value = config("openai", "gpt-5.6-terra", "high");
    assert!(
        jsonschema::validator_for(&config_schema())
            .unwrap()
            .is_valid(&value)
    );
    value["provider_options"] = json!({"codex_responses_lite":true});
    assert!(Config::parse(value.as_object().unwrap()).is_err());
}
