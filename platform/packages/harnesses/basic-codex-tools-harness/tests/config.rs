use basic_codex_tools_harness::{Config, config_schema, supported_models};
use llm_contracts::*;
use serde_json::json;
use uuid::Uuid;

#[test]
fn ordinary_responses_policy_and_native_history_work_for_both_providers() {
    for (provider, models) in supported_models() {
        for model in models {
            for effort in ["low", "medium", "high", "xhigh", "max"] {
                let config = Config::parse(json!({"model":{"provider":provider,"id":model},"reasoning_level":effort,
                    "environment":{"type":"machine","machine_id":Uuid::new_v4(),"workspace_root":"/work","path":"."}
                }).as_object().unwrap()).unwrap();
                let session = Uuid::new_v4();
                let options = config.provider_options(session).unwrap();
                assert_eq!(options, json!({"store":false,"include":["reasoning.encrypted_content"],"prompt_cache_key":session.to_string(),
                    "tool_choice":"auto","parallel_tool_calls":false,"reasoning":{"effort":effort},"text":{"verbosity":"low"}}).as_object().unwrap().clone());
                let native = json!({"output":[{"type":"reasoning","id":"rs_1","encrypted_content":"opaque","summary":[]},
                    {"type":"custom_tool_call","id":"ct_1","call_id":"call_1","name":"apply_patch","input":"*** Begin Patch\n*** Add File: a\n+x\n*** End Patch"}]});
                let request = LlmRequest {
                    model: config.model.clone(),
                    instructions: Some("Coding instructions".into()),
                    tools: vec![tool_apply_patch::definition()],
                    messages: vec![Message::Assistant(AssistantMessage {
                        id: MessageId::new("assistant").unwrap(),
                        model: config.model.clone(),
                        content: vec![],
                        native_message: native.clone(),
                        usage: None,
                        duration_ms: 0,
                        stop_reason: StopReason::ToolUse,
                        timestamp: Timestamp(0),
                    })],
                    provider_options: options,
                    metadata: Default::default(),
                };
                let body = if provider == "openai" {
                    provider_openai::build_response_request(&request)
                } else {
                    provider_chatgpt::build_response_request(&request)
                }
                .unwrap();
                assert_eq!(body["input"], native["output"]);
                assert_eq!(body["instructions"], "Coding instructions");
                assert_eq!(body["tools"][0]["type"], "custom");
                assert!(body.get("max_output_tokens").is_none());
                assert!(body.get("codex_responses_lite").is_none());
                assert_eq!(
                    body.get("stream"),
                    if provider == "chatgpt" {
                        Some(&json!(true))
                    } else {
                        None
                    }
                );
            }
        }
    }
}

#[test]
fn fireworks_and_provider_overrides_are_rejected() {
    let mut config = json!({"model":{"provider":"fireworks","id":"gpt-5.6-terra"},"reasoning_level":"high",
        "environment":{"type":"machine","machine_id":Uuid::new_v4(),"workspace_root":"/work","path":"."}});
    assert!(
        !jsonschema::validator_for(&config_schema())
            .unwrap()
            .is_valid(&config)
    );
    assert!(Config::parse(config.as_object().unwrap()).is_err());
    config["model"]["provider"] = json!("openai");
    config["provider_options"] = json!({"codex_responses_lite":true});
    assert!(Config::parse(config.as_object().unwrap()).is_err());
}
