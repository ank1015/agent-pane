use std::collections::HashSet;

use agent_contracts::SessionMessage;
use llm_contracts::{AssistantContent, AssistantMessage, Message};
use uuid::Uuid;

#[derive(Clone, Debug, PartialEq)]
pub(super) enum ResumePlan {
    CallModel,
    Complete {
        final_message_id: Uuid,
    },
    ExecuteTools {
        assistant: Box<AssistantMessage>,
        missing_tool_calls: Vec<AssistantContent>,
    },
    Continue,
}

pub(super) fn plan_turn(
    messages: &[SessionMessage],
    run_id: Uuid,
    turn_number: u32,
) -> Result<ResumePlan, TranscriptError> {
    let current = messages
        .iter()
        .filter(|message| {
            message.run_id == Some(run_id) && message.turn_number == Some(turn_number)
        })
        .collect::<Vec<_>>();
    let Some(last) = current.last() else {
        return Ok(ResumePlan::CallModel);
    };

    match &last.message {
        Message::User(_) | Message::System(_) | Message::Custom(_) => Ok(ResumePlan::CallModel),
        Message::Assistant(_) => plan_assistant(&current, current.len() - 1),
        Message::ToolResult(_) => {
            let assistant_index = current
                .iter()
                .rposition(|message| matches!(message.message, Message::Assistant(_)))
                .ok_or(TranscriptError::ToolResultWithoutAssistant)?;
            plan_assistant(&current, assistant_index)
        }
    }
}

fn plan_assistant(
    current: &[&SessionMessage],
    assistant_index: usize,
) -> Result<ResumePlan, TranscriptError> {
    let assistant_message = current[assistant_index];
    let Message::Assistant(assistant) = &assistant_message.message else {
        unreachable!("assistant index points to an assistant message")
    };
    let tool_calls = assistant
        .content
        .iter()
        .filter_map(|content| match content {
            AssistantContent::ToolCall { tool_call_id, .. } => {
                Some((tool_call_id.clone(), content.clone()))
            }
            _ => None,
        })
        .collect::<Vec<_>>();
    if tool_calls.is_empty() {
        if current[assistant_index + 1..]
            .iter()
            .any(|message| matches!(message.message, Message::ToolResult(_)))
        {
            return Err(TranscriptError::UnexpectedToolResult);
        }
        return Ok(ResumePlan::Complete {
            final_message_id: assistant_message.session_message_id,
        });
    }

    let expected = tool_calls.iter().map(|(id, _)| id).collect::<HashSet<_>>();
    let completed = current[assistant_index + 1..]
        .iter()
        .filter_map(|message| match &message.message {
            Message::ToolResult(result) => Some(&result.tool_call_id),
            _ => None,
        })
        .collect::<HashSet<_>>();
    if completed.iter().any(|id| !expected.contains(id)) {
        return Err(TranscriptError::UnexpectedToolResult);
    }
    let missing_tool_calls = tool_calls
        .into_iter()
        .filter_map(|(id, content)| (!completed.contains(&id)).then_some(content))
        .collect::<Vec<_>>();
    if missing_tool_calls.is_empty() {
        Ok(ResumePlan::Continue)
    } else {
        Ok(ResumePlan::ExecuteTools {
            assistant: Box::new(assistant.clone()),
            missing_tool_calls,
        })
    }
}

#[derive(Debug, Eq, PartialEq, thiserror::Error)]
pub(super) enum TranscriptError {
    #[error("the current turn contains a tool result without an assistant message")]
    ToolResultWithoutAssistant,
    #[error("the current turn contains a tool result that does not belong to its assistant")]
    UnexpectedToolResult,
}

#[cfg(test)]
mod tests {
    use agent_contracts::{SessionMessage, SessionMessageDelivery, SessionMessageOrigin};
    use chrono::Utc;
    use llm_contracts::Message;
    use serde_json::json;
    use uuid::Uuid;

    use super::{ResumePlan, plan_turn};

    #[test]
    fn calls_the_model_without_messages_from_the_requested_turn() {
        let run_id = Uuid::now_v7();
        let messages = vec![session_message(run_id, None, user("trigger"))];

        assert_eq!(
            plan_turn(&messages, run_id, 1).expect("plan"),
            ResumePlan::CallModel
        );
    }

    #[test]
    fn a_new_user_message_after_an_assistant_starts_another_model_call() {
        let run_id = Uuid::now_v7();
        let messages = vec![
            session_message(run_id, Some(1), assistant("assistant", &[])),
            session_message(run_id, Some(1), user("steer")),
        ];

        assert_eq!(
            plan_turn(&messages, run_id, 1).expect("plan"),
            ResumePlan::CallModel
        );
    }

    #[test]
    fn completes_from_a_committed_terminal_assistant() {
        let run_id = Uuid::now_v7();
        let messages = vec![session_message(
            run_id,
            Some(1),
            assistant("assistant", &[]),
        )];
        let final_message_id = messages[0].session_message_id;

        assert_eq!(
            plan_turn(&messages, run_id, 1).expect("plan"),
            ResumePlan::Complete { final_message_id }
        );
    }

    #[test]
    fn resumes_only_missing_tool_calls_and_continues_when_all_are_done() {
        let run_id = Uuid::now_v7();
        let assistant = assistant("assistant", &["call-1", "call-2"]);
        let messages = vec![
            session_message(run_id, Some(1), assistant),
            session_message(run_id, Some(1), tool_result("result-1", "call-1")),
        ];

        let ResumePlan::ExecuteTools {
            missing_tool_calls, ..
        } = plan_turn(&messages, run_id, 1).expect("plan")
        else {
            panic!("missing tool call should be resumed")
        };
        assert_eq!(missing_tool_calls.len(), 1);
        assert_eq!(tool_call_id(&missing_tool_calls[0]), "call-2");

        let mut complete = messages;
        complete.push(session_message(
            run_id,
            Some(1),
            tool_result("result-2", "call-2"),
        ));
        assert_eq!(
            plan_turn(&complete, run_id, 1).expect("plan"),
            ResumePlan::Continue
        );
    }

    fn session_message(run_id: Uuid, turn: Option<u32>, message: Message) -> SessionMessage {
        SessionMessage {
            session_message_id: Uuid::now_v7(),
            session_id: Uuid::nil(),
            revision: 1,
            message,
            origin: if turn.is_some() {
                SessionMessageOrigin::Harness
            } else {
                SessionMessageOrigin::External
            },
            delivery: SessionMessageDelivery::Immediate,
            run_id: turn.map(|_| run_id),
            turn_number: turn,
            created_at: Utc::now(),
            committed_at: Utc::now(),
        }
    }

    fn user(id: &str) -> Message {
        message(json!({
            "role": "user",
            "id": id,
            "timestamp": 1,
            "content": [{"type": "text", "content": "continue"}]
        }))
    }

    fn assistant(id: &str, calls: &[&str]) -> Message {
        let content = if calls.is_empty() {
            json!([{"type": "response", "response": {"content": "done"}}])
        } else {
            serde_json::Value::Array(
                calls
                    .iter()
                    .map(|call| {
                        json!({
                            "type": "tool_call",
                            "name": "read",
                            "arguments": {"path": "src/lib.rs"},
                            "tool_call_id": call
                        })
                    })
                    .collect(),
            )
        };
        message(json!({
            "role": "assistant",
            "id": id,
            "model": {"provider": "openai", "id": "gpt-5.6-sol"},
            "duration_ms": 1,
            "native_message": {"output": []},
            "content": content,
            "stop_reason": if calls.is_empty() { "stop" } else { "tool_use" },
            "timestamp": 1
        }))
    }

    fn tool_result(id: &str, call: &str) -> Message {
        message(json!({
            "role": "tool_result",
            "id": id,
            "tool_name": "read",
            "tool_call_id": call,
            "content": [{"type": "text", "content": "contents"}],
            "timestamp": 1,
            "outcome": {"status": "success"}
        }))
    }

    fn tool_call_id(content: &llm_contracts::AssistantContent) -> &str {
        let llm_contracts::AssistantContent::ToolCall { tool_call_id, .. } = content else {
            panic!("tool call")
        };
        tool_call_id.as_str()
    }

    fn message(value: serde_json::Value) -> Message {
        serde_json::from_value(value).expect("valid message")
    }
}
