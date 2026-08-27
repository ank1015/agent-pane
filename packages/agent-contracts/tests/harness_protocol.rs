use agent_contracts::{
    NewRunMessage,
    harness_protocol::{
        AppendSessionMessages, HARNESS_PROTOCOL_VERSION, HarnessCommand, HarnessOperation,
        TurnRequested, WaitRequest,
    },
};
use chrono::{Duration, Utc};
use llm_contracts::{Validate as _, validation::parse_json};
use serde_json::json;
use uuid::Uuid;

#[test]
fn turn_request_uses_active_run_context_without_worker_ownership() {
    let request: TurnRequested = parse_json(
        &json!({
            "protocol_version": HARNESS_PROTOCOL_VERSION,
            "event_id": Uuid::now_v7(),
            "emitted_at": Utc::now(),
            "run_id": Uuid::now_v7(),
            "session_id": Uuid::now_v7(),
            "turn_number": 2,
            "max_turns": 20,
            "expected_state_version": 4,
            "harness_id": "pi",
            "harness_slug": "pi",
            "harness_revision_id": "pi-2026-08-26",
            "resolved_config": {"model_id": "gpt-5.6-sol"},
            "current_session_revision": 8,
            "resume": null
        })
        .to_string(),
    )
    .expect("valid turn request");

    assert_eq!(request.turn_number, 2);
    let value = serde_json::to_value(request).expect("serialize turn request");
    assert!(value.get("lease").is_none());
    assert!(value.get("worker_instance_id").is_none());
}

#[test]
fn harness_commands_expose_only_the_four_lifecycle_operations() {
    let issued_at = Utc::now();
    let operations = [
        HarnessOperation::Complete {
            final_message_id: Uuid::now_v7(),
        },
        HarnessOperation::Continue,
        HarnessOperation::Fail {
            failure: object(json!({"code": "provider_rate_limit"})),
        },
        HarnessOperation::Wait(WaitRequest {
            wait_id: Uuid::now_v7(),
            harness_wait_id: "provider-backoff".to_owned(),
            kind: "retry".to_owned(),
            public_request: object(json!({})),
            resume_metadata: object(json!({"retry": 2})),
            expires_at: Some(issued_at + Duration::seconds(30)),
        }),
    ];

    for operation in operations {
        HarnessCommand {
            protocol_version: HARNESS_PROTOCOL_VERSION,
            command_id: Uuid::now_v7(),
            issued_at,
            run_id: Uuid::now_v7(),
            harness_slug: "pi".to_owned(),
            turn_number: 1,
            expected_state_version: 2,
            operation,
        }
        .validate()
        .expect("valid lifecycle command");
    }
}

#[test]
fn wait_expiry_must_be_after_the_command_time() {
    let issued_at = Utc::now();
    let command = HarnessCommand {
        protocol_version: HARNESS_PROTOCOL_VERSION,
        command_id: Uuid::now_v7(),
        issued_at,
        run_id: Uuid::now_v7(),
        harness_slug: "pi".to_owned(),
        turn_number: 1,
        expected_state_version: 1,
        operation: HarnessOperation::Wait(WaitRequest {
            wait_id: Uuid::now_v7(),
            harness_wait_id: "provider-backoff".to_owned(),
            kind: "retry".to_owned(),
            public_request: object(json!({})),
            resume_metadata: object(json!({})),
            expires_at: Some(issued_at),
        }),
    };

    let error = command.validate().expect_err("expired wait must fail");
    assert_eq!(error.issues[0].path, "operation.details.expires_at");
}

#[test]
fn session_message_append_is_lease_free_and_fenced_by_run_state() {
    let request = AppendSessionMessages {
        expected_state_version: 3,
        turn_number: 2,
        expected_session_revision: 7,
        messages: vec![NewRunMessage {
            session_message_id: Uuid::now_v7(),
            message: serde_json::from_value(json!({
                "role": "custom",
                "id": "checkpoint-1",
                "content": {"type": "checkpoint"},
                "timestamp": 1
            }))
            .expect("custom message"),
        }],
    };

    request.validate().expect("valid append request");
    let value = serde_json::to_value(request).expect("serialize append request");
    assert!(value.get("lease_version").is_none());
    assert_eq!(value["expected_state_version"], 3);
    assert_eq!(value["turn_number"], 2);
}

fn object(value: serde_json::Value) -> llm_contracts::JsonObject {
    value.as_object().expect("object").clone()
}
