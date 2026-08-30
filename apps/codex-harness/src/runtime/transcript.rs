use std::collections::{HashMap, HashSet};

use agent_contracts::SessionMessage;
use llm_contracts::{AssistantContent, AssistantMessage, Message};
use uuid::Uuid;

pub const CODEX_PRIMARY_CALL_STARTED_TAG: &str = "codex.primary_call_started";
pub const CODEX_CONTEXT_OVERFLOW_TAG: &str = "codex.context_overflow";

/// Durable action implied by messages already committed for one Agent turn.
#[derive(Clone, Debug, PartialEq)]
pub enum ResumePlan {
    /// No assistant response has been committed for this logical turn.
    CallModel,
    /// Sampling began but no assistant was committed. Reissuing the call would
    /// violate the one-primary-call invariant.
    PrimaryModelCallInterrupted,
    /// The assistant requested tools. Any missing result is repaired as a
    /// prompt-only `aborted` output during context normalization; historical
    /// calls are never dispatched again.
    Continue,
    /// The committed assistant response is terminal and can be completed again safely.
    Complete { final_message_id: Uuid },
}

/// Plans one broker delivery without ever scheduling a second primary model
/// call for the same `(run_id, turn_number)`.
pub fn plan_turn(
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

    let assistant_indices = current
        .iter()
        .enumerate()
        .filter_map(|(index, message)| {
            matches!(message.message, Message::Assistant(_)).then_some(index)
        })
        .collect::<Vec<_>>();
    match assistant_indices.as_slice() {
        [] => plan_before_assistant(&current),
        [assistant_index] => plan_after_assistant(&current, *assistant_index),
        _ => Err(TranscriptError::MultipleAssistantMessages),
    }
}

fn plan_before_assistant(current: &[&SessionMessage]) -> Result<ResumePlan, TranscriptError> {
    if current
        .iter()
        .any(|message| matches!(message.message, Message::ToolResult(_)))
    {
        return Err(TranscriptError::ToolResultWithoutAssistant);
    }
    // Context overflow is a completed, model-less turn outcome. Replaying the
    // same broker delivery must return Continue again, not reinterpret the
    // preceding primary-call marker as an interrupted sample.
    if current.iter().any(|message| {
        matches!(
            &message.message,
            Message::Custom(custom)
                if custom.tag.as_deref() == Some(CODEX_CONTEXT_OVERFLOW_TAG)
        )
    }) {
        return Ok(ResumePlan::Continue);
    }
    if current.iter().any(|message| {
        matches!(
            &message.message,
            Message::Custom(custom)
                if custom.tag.as_deref() == Some(CODEX_PRIMARY_CALL_STARTED_TAG)
        )
    }) {
        return Ok(ResumePlan::PrimaryModelCallInterrupted);
    }
    Ok(ResumePlan::CallModel)
}

fn plan_after_assistant(
    current: &[&SessionMessage],
    assistant_index: usize,
) -> Result<ResumePlan, TranscriptError> {
    if current[..assistant_index]
        .iter()
        .any(|message| matches!(message.message, Message::ToolResult(_)))
    {
        return Err(TranscriptError::ToolResultBeforeAssistant);
    }
    let assistant_message = current[assistant_index];
    let Message::Assistant(assistant) = &assistant_message.message else {
        unreachable!("assistant index points to an assistant message")
    };
    let tool_calls = collect_tool_calls(assistant)?;

    let mut completed = HashSet::new();
    for message in &current[assistant_index + 1..] {
        let Message::ToolResult(result) = &message.message else {
            return Err(TranscriptError::UnexpectedMessageAfterAssistant);
        };
        let tool_call_id = result.tool_call_id.as_str();
        let Some(expected_name) = tool_calls.get(tool_call_id).map(|call| call.name) else {
            return Err(TranscriptError::UnexpectedToolResult(
                tool_call_id.to_owned(),
            ));
        };
        if expected_name != result.tool_name {
            return Err(TranscriptError::ToolNameMismatch {
                tool_call_id: tool_call_id.to_owned(),
                expected: expected_name.to_owned(),
                actual: result.tool_name.clone(),
            });
        }
        if !completed.insert(tool_call_id.to_owned()) {
            return Err(TranscriptError::DuplicateToolResult(
                tool_call_id.to_owned(),
            ));
        }
    }

    if tool_calls.is_empty() {
        return Ok(ResumePlan::Complete {
            final_message_id: assistant_message.session_message_id,
        });
    }

    Ok(ResumePlan::Continue)
}

struct ToolCall<'a> {
    name: &'a str,
}

fn collect_tool_calls(
    assistant: &AssistantMessage,
) -> Result<HashMap<&str, ToolCall<'_>>, TranscriptError> {
    let mut calls = HashMap::new();
    for content in &assistant.content {
        let AssistantContent::ToolCall {
            name, tool_call_id, ..
        } = content
        else {
            continue;
        };
        if calls
            .insert(tool_call_id.as_str(), ToolCall { name })
            .is_some()
        {
            return Err(TranscriptError::DuplicateToolCall(
                tool_call_id.as_str().to_owned(),
            ));
        }
    }
    Ok(calls)
}

#[derive(Debug, Eq, PartialEq, thiserror::Error)]
pub enum TranscriptError {
    #[error("the current turn contains more than one assistant message")]
    MultipleAssistantMessages,
    #[error("the current turn contains a tool result without an assistant message")]
    ToolResultWithoutAssistant,
    #[error("the current turn contains a tool result before its assistant message")]
    ToolResultBeforeAssistant,
    #[error("the current turn contains a non-tool-result message after its assistant message")]
    UnexpectedMessageAfterAssistant,
    #[error("assistant contains duplicate tool call ID {0:?}")]
    DuplicateToolCall(String),
    #[error("the current turn contains an unexpected tool result for call {0:?}")]
    UnexpectedToolResult(String),
    #[error("the current turn contains duplicate results for tool call {0:?}")]
    DuplicateToolResult(String),
    #[error(
        "tool result for call {tool_call_id:?} names {actual:?}, but the assistant requested {expected:?}"
    )]
    ToolNameMismatch {
        tool_call_id: String,
        expected: String,
        actual: String,
    },
}

#[cfg(test)]
mod tests {
    use agent_contracts::{SessionMessage, SessionMessageDelivery, SessionMessageOrigin};
    use chrono::Utc;
    use llm_contracts::Message;
    use serde_json::json;
    use uuid::Uuid;

    use super::{
        CODEX_CONTEXT_OVERFLOW_TAG, CODEX_PRIMARY_CALL_STARTED_TAG, ResumePlan, TranscriptError,
        plan_turn,
    };

    #[test]
    fn calls_model_when_this_turn_has_no_assistant() {
        let run_id = Uuid::now_v7();
        let other_run = Uuid::now_v7();
        let messages = vec![
            session_message(other_run, Some(1), assistant("old", &[])),
            session_message(run_id, None, user("trigger")),
            session_message(run_id, Some(1), custom("compaction")),
        ];
        assert_eq!(
            plan_turn(&messages, run_id, 1).expect("plan"),
            ResumePlan::CallModel
        );
    }

    #[test]
    fn distinguishes_interrupted_primary_calls_from_committed_overflow_outcomes() {
        let run_id = Uuid::now_v7();
        let started = vec![session_message(
            run_id,
            Some(1),
            tagged_custom(CODEX_PRIMARY_CALL_STARTED_TAG),
        )];
        assert_eq!(
            plan_turn(&started, run_id, 1).expect("plan"),
            ResumePlan::PrimaryModelCallInterrupted
        );

        let overflow = vec![
            session_message(
                run_id,
                Some(1),
                tagged_custom(CODEX_PRIMARY_CALL_STARTED_TAG),
            ),
            session_message(run_id, Some(1), tagged_custom(CODEX_CONTEXT_OVERFLOW_TAG)),
        ];
        assert_eq!(
            plan_turn(&overflow, run_id, 1).expect("plan"),
            ResumePlan::Continue
        );
    }

    #[test]
    fn completes_from_committed_terminal_assistant() {
        let run_id = Uuid::now_v7();
        let messages = vec![session_message(
            run_id,
            Some(1),
            assistant("assistant", &[]),
        )];
        assert_eq!(
            plan_turn(&messages, run_id, 1).expect("plan"),
            ResumePlan::Complete {
                final_message_id: messages[0].session_message_id
            }
        );
    }

    #[test]
    fn continues_without_reexecuting_partial_or_missing_tool_results() {
        let run_id = Uuid::now_v7();
        let messages = vec![
            session_message(
                run_id,
                Some(1),
                assistant_with_tools(
                    "assistant",
                    &[("call-1", "exec"), ("call-2", "wait"), ("call-3", "exec")],
                ),
            ),
            session_message(run_id, Some(1), tool_result("result-2", "call-2", "wait")),
        ];

        assert_eq!(
            plan_turn(&messages, run_id, 1).expect("plan"),
            ResumePlan::Continue
        );

        let all_missing = vec![session_message(
            run_id,
            Some(1),
            assistant_with_tools("assistant", &[("call-1", "exec"), ("call-2", "wait")]),
        )];
        assert_eq!(
            plan_turn(&all_missing, run_id, 1).expect("plan"),
            ResumePlan::Continue
        );
    }

    #[test]
    fn continues_when_every_tool_result_is_committed() {
        let run_id = Uuid::now_v7();
        let messages = vec![
            session_message(
                run_id,
                Some(2),
                assistant_with_tools("assistant", &[("call-1", "exec"), ("call-2", "wait")]),
            ),
            session_message(run_id, Some(2), tool_result("result-1", "call-1", "exec")),
            session_message(run_id, Some(2), tool_result("result-2", "call-2", "wait")),
        ];
        assert_eq!(
            plan_turn(&messages, run_id, 2).expect("plan"),
            ResumePlan::Continue
        );
    }

    #[test]
    fn never_calls_model_again_after_an_assistant_exists() {
        let run_id = Uuid::now_v7();
        let messages = vec![
            session_message(run_id, Some(1), assistant("assistant", &[])),
            session_message(run_id, Some(1), user("late-user")),
        ];
        assert_eq!(
            plan_turn(&messages, run_id, 1),
            Err(TranscriptError::UnexpectedMessageAfterAssistant)
        );
    }

    #[test]
    fn rejects_ambiguous_or_corrupt_tool_correlations() {
        let run_id = Uuid::now_v7();

        let duplicate_calls = vec![session_message(
            run_id,
            Some(1),
            assistant_with_tools("assistant", &[("same", "exec"), ("same", "wait")]),
        )];
        assert_eq!(
            plan_turn(&duplicate_calls, run_id, 1),
            Err(TranscriptError::DuplicateToolCall("same".to_owned()))
        );

        let wrong_name = vec![
            session_message(
                run_id,
                Some(1),
                assistant_with_tools("assistant", &[("call-1", "exec")]),
            ),
            session_message(run_id, Some(1), tool_result("result", "call-1", "wait")),
        ];
        assert_eq!(
            plan_turn(&wrong_name, run_id, 1),
            Err(TranscriptError::ToolNameMismatch {
                tool_call_id: "call-1".to_owned(),
                expected: "exec".to_owned(),
                actual: "wait".to_owned(),
            })
        );

        let duplicate_results = vec![
            session_message(
                run_id,
                Some(1),
                assistant_with_tools("assistant", &[("call-1", "exec")]),
            ),
            session_message(run_id, Some(1), tool_result("result-1", "call-1", "exec")),
            session_message(run_id, Some(1), tool_result("result-2", "call-1", "exec")),
        ];
        assert_eq!(
            plan_turn(&duplicate_results, run_id, 1),
            Err(TranscriptError::DuplicateToolResult("call-1".to_owned()))
        );
    }

    #[test]
    fn rejects_multiple_assistants_and_orphan_results() {
        let run_id = Uuid::now_v7();
        let multiple = vec![
            session_message(run_id, Some(1), assistant("one", &[])),
            session_message(run_id, Some(1), assistant("two", &[])),
        ];
        assert_eq!(
            plan_turn(&multiple, run_id, 1),
            Err(TranscriptError::MultipleAssistantMessages)
        );
        let orphan = vec![session_message(
            run_id,
            Some(1),
            tool_result("result", "call-1", "exec"),
        )];
        assert_eq!(
            plan_turn(&orphan, run_id, 1),
            Err(TranscriptError::ToolResultWithoutAssistant)
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

    fn custom(id: &str) -> Message {
        message(json!({
            "role": "custom",
            "id": id,
            "tag": "codex_compaction",
            "timestamp": 1,
            "content": {"summary": "summary"}
        }))
    }

    fn tagged_custom(tag: &str) -> Message {
        message(json!({
            "role": "custom",
            "id": format!("custom-{tag}"),
            "content": {},
            "tag": tag,
            "timestamp": 1
        }))
    }

    fn assistant(id: &str, calls: &[&str]) -> Message {
        let calls = calls.iter().map(|call| (*call, "exec")).collect::<Vec<_>>();
        assistant_with_tools(id, &calls)
    }

    fn assistant_with_tools(id: &str, calls: &[(&str, &str)]) -> Message {
        let content = if calls.is_empty() {
            json!([{"type": "response", "response": {"content": "done"}}])
        } else {
            serde_json::Value::Array(
                calls
                    .iter()
                    .map(|(call, name)| {
                        json!({
                            "type": "tool_call",
                            "name": name,
                            "arguments": if *name == "exec" { json!("text(1)") } else { json!({"cell_id": "cell-1"}) },
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

    fn tool_result(id: &str, call: &str, name: &str) -> Message {
        message(json!({
            "role": "tool_result",
            "id": id,
            "tool_name": name,
            "tool_call_id": call,
            "content": [{"type": "text", "content": "contents"}],
            "timestamp": 1,
            "outcome": {"status": "success"}
        }))
    }

    fn message(value: serde_json::Value) -> Message {
        serde_json::from_value(value).expect("valid message")
    }
}
