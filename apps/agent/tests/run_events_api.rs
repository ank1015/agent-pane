mod support;

use agent::{
    Database,
    execution::{HarnessEventIngestOutcome, apply_harness_command, ingest_harness_event},
};
use agent_contracts::{
    HARNESS_PROTOCOL_VERSION, HarnessCommand, HarnessOperation, HarnessRunEvent,
    HarnessRunEventData, WaitRequest,
};
use chrono::{Duration, Utc};
use futures_util::StreamExt as _;
use llm_contracts::JsonObject;
use reqwest::{Method, StatusCode, header::CONTENT_TYPE};
use serde_json::{Value, json};
use support::{TestApp, test_app, unique};
use uuid::Uuid;

#[tokio::test]
#[ignore = "requires AGENT_TEST_DATABASE_URL"]
async fn run_events_replay_harness_progress_and_close_after_terminal_state() {
    let app = test_app().await;
    let fixture = start_run(&app).await;
    let database = Database::from_pool(app.pool.clone());

    let started = HarnessRunEvent {
        protocol_version: HARNESS_PROTOCOL_VERSION,
        event_id: Uuid::now_v7(),
        emitted_at: Utc::now(),
        run_id: fixture.run_id,
        harness_slug: fixture.harness_slug.clone(),
        turn_number: 1,
        expected_state_version: 1,
        event: HarnessRunEventData::TurnStarted,
    };
    assert_eq!(
        ingest_harness_event(&database, &started).await.unwrap(),
        HarnessEventIngestOutcome::Applied
    );
    assert_eq!(
        ingest_harness_event(&database, &started).await.unwrap(),
        HarnessEventIngestOutcome::Duplicate
    );

    let stale = HarnessRunEvent {
        event_id: Uuid::now_v7(),
        expected_state_version: 99,
        event: HarnessRunEventData::Progress {
            name: "model.call.started".to_owned(),
            data: JsonObject::new(),
        },
        ..started.clone()
    };
    assert_eq!(
        ingest_harness_event(&database, &stale).await.unwrap(),
        HarnessEventIngestOutcome::Rejected
    );

    let fail = HarnessCommand {
        protocol_version: HARNESS_PROTOCOL_VERSION,
        command_id: Uuid::now_v7(),
        issued_at: Utc::now(),
        run_id: fixture.run_id,
        harness_slug: fixture.harness_slug,
        turn_number: 1,
        expected_state_version: 1,
        operation: HarnessOperation::Fail {
            failure: object(json!({"code": "test_failure"})),
        },
    };
    apply_harness_command(&database, &fail, &serde_json::to_vec(&fail).unwrap())
        .await
        .unwrap();

    let history = app
        .control(
            Method::GET,
            &format!("/v1/runs/{}/events?after_sequence=2", fixture.run_id),
        )
        .send()
        .await
        .unwrap();
    assert_eq!(history.status(), StatusCode::OK);
    let history: Value = history.json().await.unwrap();
    assert_eq!(history["items"].as_array().unwrap().len(), 3);
    assert_eq!(history["items"][0]["type"], "turn_started");
    assert_eq!(history["items"][1]["type"], "turn_ended");
    assert_eq!(history["items"][2]["type"], "run_failed");

    let stream = app
        .control(
            Method::GET,
            &format!("/v1/runs/{}/events/stream?after_sequence=2", fixture.run_id),
        )
        .send()
        .await
        .unwrap();
    assert_eq!(stream.status(), StatusCode::OK);
    assert_eq!(
        stream.headers().get(CONTENT_TYPE).unwrap(),
        "text/event-stream"
    );
    let body = stream.text().await.unwrap();
    assert!(body.contains("id: 3\nevent: turn.started"));
    assert!(body.contains("id: 4\nevent: turn.ended"));
    assert!(body.contains("id: 5\nevent: run.failed"));
}

#[tokio::test]
#[ignore = "requires AGENT_TEST_DATABASE_URL"]
async fn mid_turn_stream_delivers_events_written_by_another_database_client() {
    let app = test_app().await;
    let fixture = start_run(&app).await;
    let response = app
        .control(
            Method::GET,
            &format!("/v1/runs/{}/events/stream?after_sequence=2", fixture.run_id),
        )
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(response.headers()["x-accel-buffering"], "no");

    let event = HarnessRunEvent {
        protocol_version: HARNESS_PROTOCOL_VERSION,
        event_id: Uuid::now_v7(),
        emitted_at: Utc::now(),
        run_id: fixture.run_id,
        harness_slug: fixture.harness_slug,
        turn_number: 1,
        expected_state_version: 1,
        event: HarnessRunEventData::Progress {
            name: "model.call.started".to_owned(),
            data: object(json!({"model": "test-model"})),
        },
    };
    assert_eq!(
        ingest_harness_event(&Database::from_pool(app.pool.clone()), &event)
            .await
            .unwrap(),
        HarnessEventIngestOutcome::Applied
    );

    let mut body = String::new();
    let mut stream = response.bytes_stream();
    tokio::time::timeout(std::time::Duration::from_secs(5), async {
        while let Some(chunk) = stream.next().await {
            body.push_str(std::str::from_utf8(&chunk.unwrap()).unwrap());
            if body.contains("event: progress") {
                break;
            }
        }
    })
    .await
    .expect("live SSE event");
    assert!(body.contains("id: 3\nevent: progress"));
    assert!(body.contains("model.call.started"));
}

#[tokio::test]
#[ignore = "requires AGENT_TEST_DATABASE_URL"]
async fn lifecycle_events_cover_continue_wait_resume_and_abort() {
    let app = test_app().await;
    let fixture = start_run(&app).await;
    let database = Database::from_pool(app.pool.clone());

    apply(
        &database,
        HarnessCommand {
            protocol_version: HARNESS_PROTOCOL_VERSION,
            command_id: Uuid::now_v7(),
            issued_at: Utc::now(),
            run_id: fixture.run_id,
            harness_slug: fixture.harness_slug.clone(),
            turn_number: 1,
            expected_state_version: 1,
            operation: HarnessOperation::Continue,
        },
    )
    .await;

    let wait_id = Uuid::now_v7();
    apply(
        &database,
        HarnessCommand {
            protocol_version: HARNESS_PROTOCOL_VERSION,
            command_id: Uuid::now_v7(),
            issued_at: Utc::now(),
            run_id: fixture.run_id,
            harness_slug: fixture.harness_slug,
            turn_number: 2,
            expected_state_version: 2,
            operation: HarnessOperation::Wait(WaitRequest {
                wait_id,
                harness_wait_id: "user-approval".to_owned(),
                kind: "approval".to_owned(),
                public_request: object(json!({"question": "Continue?"})),
                resume_metadata: JsonObject::new(),
                expires_at: Some(Utc::now() + Duration::minutes(5)),
            }),
        },
    )
    .await;

    let resolved = app
        .control(
            Method::POST,
            &format!("/v1/runs/{}/waits/{wait_id}/resolve", fixture.run_id),
        )
        .json(&json!({"expected_state_version": 3, "resolution": {"approved": true}}))
        .send()
        .await
        .unwrap();
    assert_eq!(resolved.status(), StatusCode::OK);

    let abort_id = Uuid::now_v7();
    let aborted = app
        .control(Method::POST, &format!("/v1/runs/{}/abort", fixture.run_id))
        .json(&json!({
            "abort_id": abort_id,
            "expected_state_version": 4,
            "reason": "test finished",
            "payload": {}
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(aborted.status(), StatusCode::ACCEPTED);

    let history: Value = app
        .control(Method::GET, &format!("/v1/runs/{}/events", fixture.run_id))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let events = history["items"].as_array().unwrap();
    assert_eq!(events.len(), 10);
    assert_eq!(
        events
            .iter()
            .map(|event| event["type"].as_str().unwrap())
            .collect::<Vec<_>>(),
        [
            "run_started",
            "turn_requested",
            "turn_ended",
            "turn_requested",
            "turn_ended",
            "run_waiting",
            "run_resumed",
            "turn_requested",
            "turn_ended",
            "run_aborted",
        ]
    );
    assert!(events.iter().enumerate().all(
        |(index, event)| event["sequence"].as_u64() == Some(u64::try_from(index + 1).unwrap())
    ));
    assert_eq!(events[2]["details"]["reason"], "continued");
    assert_eq!(events[4]["details"]["reason"], "waiting");
    assert_eq!(events[8]["details"]["reason"], "aborted");
    assert_eq!(events[9]["details"]["abort_id"], abort_id.to_string());
}

struct Fixture {
    run_id: Uuid,
    harness_slug: String,
}

async fn start_run(app: &TestApp) -> Fixture {
    let harness_id = unique("event-harness");
    let harness_slug = unique("event");
    let revision_id = unique("event-revision");
    assert_eq!(
        app.control(Method::POST, "/v1/harnesses")
            .json(&json!({"harness_id": harness_id, "slug": harness_slug, "display_name": "Event test"}))
            .send().await.unwrap().status(),
        StatusCode::CREATED
    );
    assert_eq!(
        app.control(Method::POST, &format!("/v1/harnesses/{harness_id}/revisions"))
            .json(&json!({"harness_revision_id": revision_id, "revision": "v1", "contract_version": 1, "default_config": {}, "config_schema": null}))
            .send().await.unwrap().status(),
        StatusCode::CREATED
    );
    assert_eq!(
        app.control(
            Method::PUT,
            &format!("/v1/harnesses/{harness_id}/active-revision")
        )
        .json(&json!({"harness_revision_id": revision_id}))
        .send()
        .await
        .unwrap()
        .status(),
        StatusCode::OK
    );
    assert_eq!(
        app.control(Method::PUT, &format!("/v1/harnesses/{harness_id}/enabled"))
            .json(&json!({"enabled": true}))
            .send()
            .await
            .unwrap()
            .status(),
        StatusCode::OK
    );
    let session_id = Uuid::now_v7();
    assert_eq!(
        app.control(Method::POST, "/v1/sessions")
            .json(&json!({"session_id": session_id}))
            .send()
            .await
            .unwrap()
            .status(),
        StatusCode::CREATED
    );
    let run_id = Uuid::now_v7();
    let message_id = Uuid::now_v7();
    let response = app
        .control(Method::POST, &format!("/v1/sessions/{session_id}/runs"))
        .json(&json!({
            "run_id": run_id,
            "input": {"session_message_id": message_id, "message": user_message(message_id)},
            "harness": {"selection": "active_revision", "harness_id": harness_id},
            "config_override": {}, "limits": {"max_turns": 5}, "expected_session_revision": 0
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::ACCEPTED);
    Fixture {
        run_id,
        harness_slug,
    }
}

fn object(value: Value) -> JsonObject {
    value.as_object().unwrap().clone()
}

async fn apply(database: &Database, command: HarnessCommand) {
    apply_harness_command(database, &command, &serde_json::to_vec(&command).unwrap())
        .await
        .unwrap();
}

fn user_message(id: Uuid) -> Value {
    json!({"role":"user","id":id.to_string(),"timestamp":1,"content":[{"type":"text","content":"start"}]})
}
