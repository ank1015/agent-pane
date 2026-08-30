use std::collections::BTreeMap;

use agent_contracts::SessionMessage;
use llm_contracts::{LlmRequest, Validate, ValidationError};
use uuid::Uuid;

use super::{
    CodexEnvironmentSnapshot, CodexHarnessConfig, ContextNormalizationError, generate_instructions,
    model_visible_tool_definitions, normalize_session_messages, resolve_model_config,
};

/// Deterministically forms one primary Codex model request. All machine- and
/// clock-dependent values must already be frozen in `environment`.
pub fn form_main_request(
    config: &CodexHarnessConfig,
    session_id: Uuid,
    environment: &CodexEnvironmentSnapshot,
    session_messages: &[SessionMessage],
) -> Result<LlmRequest, ContextFormationError> {
    let model = resolve_model_config(config, session_id);
    let mut messages = normalize_session_messages(session_id, session_messages, config.provider)?;
    let latest_compaction = messages.iter().rposition(is_native_compaction);
    let environment_index = latest_compaction.map_or_else(
        || {
            messages
                .iter()
                .rposition(|message| matches!(message, llm_contracts::Message::User(_)))
                .unwrap_or(messages.len())
        },
        |compaction_index| {
            messages[compaction_index + 1..]
                .iter()
                .rposition(|message| matches!(message, llm_contracts::Message::User(_)))
                .map_or(compaction_index, |relative| compaction_index + 1 + relative)
        },
    );
    messages.insert(environment_index, environment.as_message(session_id));
    let request = LlmRequest {
        model: model.model,
        instructions: Some(generate_instructions(
            config.external_prompt.as_deref(),
            config.is_replaced,
        )),
        messages,
        tools: model_visible_tool_definitions(),
        provider_options: model.provider_options,
        metadata: BTreeMap::new(),
    };
    request
        .validate()
        .map_err(ContextFormationError::InvalidRequest)?;
    Ok(request)
}

fn is_native_compaction(message: &llm_contracts::Message) -> bool {
    let llm_contracts::Message::Custom(message) = message else {
        return false;
    };
    message
        .content
        .get("items")
        .and_then(serde_json::Value::as_array)
        .is_some_and(|items| {
            items.iter().any(|item| {
                matches!(
                    item.get("type").and_then(serde_json::Value::as_str),
                    Some("compaction" | "compaction_summary")
                )
            })
        })
}

#[derive(Debug, thiserror::Error)]
pub enum ContextFormationError {
    #[error("could not normalize Codex session history")]
    Normalize(#[from] ContextNormalizationError),
    #[error("formed Codex model request is invalid")]
    InvalidRequest(#[source] ValidationError),
}

#[cfg(test)]
mod tests {
    use agent_contracts::{SessionMessage, SessionMessageDelivery, SessionMessageOrigin};
    use chrono::Utc;
    use llm_contracts::{
        ContentPart, JsonObject, Message, MessageId, TextContent, Timestamp, UserMessage,
    };
    use serde_json::{Value, json};
    use uuid::Uuid;

    use super::form_main_request;
    use crate::runtime::{
        BASE_INSTRUCTIONS, CODEX_COMPACTION_SCHEMA_VERSION, CodexCompactionMessageContent,
        CodexCompactionTrigger, CodexEnvironmentSnapshot, CodexHarnessConfig, CodexProvider,
        create_codex_compaction_message,
    };

    #[test]
    fn forms_openai_wire_request_with_codex_context_and_options() {
        let session_id = Uuid::now_v7();
        let request = request("openai", "gpt-5.6-sol", session_id);
        let body = provider_openai::build_response_request(&request).expect("OpenAI request");
        assert_eq!(body["model"], json!("gpt-5.6-sol"));
        assert!(body.get("instructions").is_none());
        assert!(body.get("tools").is_none());
        assert_eq!(
            body["reasoning"],
            json!({"effort": "high", "context": "all_turns"})
        );
        assert_eq!(body["text"], json!({"verbosity": "low"}));
        assert_eq!(body["store"], json!(false));
        assert_eq!(body["tool_choice"], json!("auto"));
        assert_eq!(body["parallel_tool_calls"], json!(false));
        assert_eq!(body["prompt_cache_key"], json!(session_id));
        assert_eq!(body["input"][0]["type"], json!("additional_tools"));
        assert_eq!(body["input"][0]["role"], json!("developer"));
        assert_eq!(body["input"][0]["tools"][0]["type"], json!("namespace"));
        assert_eq!(body["input"][0]["tools"][0]["name"], json!("functions"));
        assert_eq!(
            body["input"][0]["tools"][0]["tools"]
                .as_array()
                .expect("namespaced tools")
                .len(),
            2
        );
        assert_eq!(
            body["input"][0]["tools"][0]["tools"][0]["name"],
            json!("exec")
        );
        assert_eq!(
            body["input"][0]["tools"][0]["tools"][1]["name"],
            json!("wait")
        );
        assert_eq!(body["input"][1]["type"], json!("message"));
        assert_eq!(body["input"][1]["role"], json!("developer"));
        assert_eq!(
            body["input"][1]["content"][0]["text"],
            json!(BASE_INSTRUCTIONS)
        );
        assert_eq!(body["input"][2]["role"], json!("user"));
        assert!(
            body["input"][2]["content"][0]["text"]
                .as_str()
                .expect("environment text")
                .contains("<environment_context>")
        );
        assert_eq!(body["input"][3]["content"][0]["text"], json!("Fix it."));
        assert!(body.get("max_output_tokens").is_none());
    }

    #[test]
    fn forms_chatgpt_wire_request_for_terra_and_luna() {
        let session_id = Uuid::now_v7();
        for model in ["gpt-5.6-terra", "gpt-5.6-luna"] {
            let request = request("chatgpt", model, session_id);
            let body = provider_chatgpt::build_response_request(&request).expect("ChatGPT request");
            assert_eq!(body["model"], json!(model));
            assert_eq!(
                body["reasoning"],
                json!({"effort": "high", "context": "all_turns"})
            );
            assert_eq!(body["stream"], json!(true));
            assert_eq!(body["include"], json!(["reasoning.encrypted_content"]));
            assert!(body.get("instructions").is_none());
            assert!(body.get("tools").is_none());
            assert_eq!(body["input"][0]["type"], json!("additional_tools"));
            assert_eq!(
                body["input"][0]["tools"][0]["tools"]
                    .as_array()
                    .expect("namespaced tools")
                    .len(),
                2
            );
            assert!(body.get("max_output_tokens").is_none());
        }
    }

    #[test]
    fn reinjects_environment_immediately_before_compaction_checkpoint() {
        let session_id = Uuid::now_v7();
        let config = CodexHarnessConfig::from_resolved(&resolved("openai", "gpt-5.6-sol"))
            .expect("valid config");
        let environment = CodexEnvironmentSnapshot::new(
            "/workspace/project",
            Some("zsh".to_owned()),
            "2026-08-30",
            "Asia/Kolkata",
            vec!["/workspace".to_owned()],
            Timestamp(1),
        )
        .expect("environment");
        let retained_user = Message::User(UserMessage {
            id: MessageId::new("retained-user").expect("message ID"),
            timestamp: Timestamp(2),
            content: vec![ContentPart::Text(TextContent {
                content: "Retained".to_owned(),
                metadata: None,
            })],
        });
        let checkpoint = create_codex_compaction_message(
            MessageId::new("checkpoint").expect("message ID"),
            Timestamp(3),
            CodexCompactionMessageContent {
                version: CODEX_COMPACTION_SCHEMA_VERSION,
                provider: CodexProvider::OpenAi,
                trigger: CodexCompactionTrigger::AutomaticLimit,
                retained_messages: vec![retained_user],
                items: vec![json!({
                    "type": "compaction",
                    "encrypted_content": "opaque"
                })],
                active_context_tokens: 10,
                usage: None,
            },
        )
        .expect("checkpoint");
        let messages = vec![SessionMessage {
            session_message_id: Uuid::now_v7(),
            session_id,
            revision: 1,
            message: Message::Custom(checkpoint),
            origin: SessionMessageOrigin::Harness,
            delivery: SessionMessageDelivery::Immediate,
            run_id: Some(Uuid::now_v7()),
            turn_number: Some(1),
            created_at: Utc::now(),
            committed_at: Utc::now(),
        }];
        let request =
            form_main_request(&config, session_id, &environment, &messages).expect("main request");
        let body = provider_openai::build_response_request(&request).expect("OpenAI request");
        let input = body["input"].as_array().expect("input");
        let compaction_index = input
            .iter()
            .position(|item| item["type"] == "compaction")
            .expect("compaction item");
        assert!(
            input[compaction_index - 1]["content"][0]["text"]
                .as_str()
                .expect("environment text")
                .contains("<environment_context>")
        );
    }

    fn request(provider: &str, model: &str, session_id: Uuid) -> llm_contracts::LlmRequest {
        let config =
            CodexHarnessConfig::from_resolved(&resolved(provider, model)).expect("valid config");
        let environment = CodexEnvironmentSnapshot::new(
            "/workspace/project",
            Some("zsh".to_owned()),
            "2026-08-30",
            "Asia/Kolkata",
            vec!["/workspace".to_owned()],
            Timestamp(1),
        )
        .expect("environment");
        let messages = vec![SessionMessage {
            session_message_id: Uuid::now_v7(),
            session_id,
            revision: 1,
            message: Message::User(UserMessage {
                id: MessageId::new("user-1").expect("message ID"),
                timestamp: Timestamp(2),
                content: vec![ContentPart::Text(TextContent {
                    content: "Fix it.".to_owned(),
                    metadata: None,
                })],
            }),
            origin: SessionMessageOrigin::External,
            delivery: SessionMessageDelivery::Immediate,
            run_id: None,
            turn_number: None,
            created_at: Utc::now(),
            committed_at: Utc::now(),
        }];
        form_main_request(&config, session_id, &environment, &messages).expect("request")
    }

    fn resolved(provider: &str, model: &str) -> JsonObject {
        let Value::Object(value) = json!({
            "provider": provider,
            "model_id": model,
            "reasoning_level": "high",
            "execution": {
                "machine_id": "machine-a",
                "workspace_root_id": "root",
                "cwd": "project"
            }
        }) else {
            unreachable!()
        };
        value
    }
}
