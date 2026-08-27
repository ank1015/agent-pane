mod support;

use agent::execution::{apply_harness_command, expire_waits_once};
use agent_contracts::{
    HARNESS_PROTOCOL_VERSION, HarnessCommand, HarnessCommandOutcome, HarnessOperation,
    TurnRequested, WaitRequest,
};
use chrono::{Duration, Utc};
use reqwest::{Method, StatusCode};
use serde_json::{Value, json};
use sqlx::types::Json;
use support::{TestApp, test_app, unique};
use uuid::Uuid;

#[tokio::test]
#[ignore = "requires AGENT_TEST_DATABASE_URL"]
async fn commands_are_transactional_idempotent_and_harness_owned() {
    let app = test_app().await;
    let fixture = start_run(&app).await;
    let event = latest_turn(&app, fixture.run_id).await;
    assert_eq!(event.turn_number, 1);
    assert_eq!(event.expected_state_version, 1);

    let command = HarnessCommand {
        protocol_version: HARNESS_PROTOCOL_VERSION,
        command_id: Uuid::now_v7(),
        issued_at: Utc::now(),
        run_id: fixture.run_id,
        harness_slug: fixture.harness_slug.clone(),
        turn_number: 1,
        expected_state_version: 1,
        operation: HarnessOperation::Continue,
    };
    let raw = serde_json::to_vec(&command).unwrap();
    let first = apply_harness_command(
        &agent::Database::from_pool(app.pool.clone()),
        &command,
        &raw,
    )
    .await
    .unwrap();
    assert!(matches!(first.outcome, HarnessCommandOutcome::Applied(_)));
    let continued_events: Vec<String> = sqlx::query_scalar(
        "select subject from broker_outbox
         where payload->>'command_id' = $1
            or (payload->>'run_id' = $2 and payload->>'expected_state_version' = '2')
         order by created_at, event_id",
    )
    .bind(command.command_id.to_string())
    .bind(fixture.run_id.to_string())
    .fetch_all(&app.pool)
    .await
    .unwrap();
    assert_eq!(continued_events.len(), 2);
    assert!(continued_events[0].ends_with(".result.v1"));
    assert!(continued_events[1].ends_with(".turn.requested.v1"));
    let duplicate = apply_harness_command(
        &agent::Database::from_pool(app.pool.clone()),
        &command,
        &raw,
    )
    .await
    .unwrap();
    assert!(matches!(
        duplicate.outcome,
        HarnessCommandOutcome::Duplicate(_)
    ));

    let run: (String, i32, i64) =
        sqlx::query_as("select status, current_turn, state_version from runs where run_id = $1")
            .bind(fixture.run_id)
            .fetch_one(&app.pool)
            .await
            .unwrap();
    assert_eq!(run, ("active".to_owned(), 2, 2));
    let receipts: i64 =
        sqlx::query_scalar("select count(*) from broker_inbox where command_id = $1")
            .bind(command.command_id)
            .fetch_one(&app.pool)
            .await
            .unwrap();
    assert_eq!(receipts, 1);

    let fail = HarnessCommand {
        protocol_version: HARNESS_PROTOCOL_VERSION,
        command_id: Uuid::now_v7(),
        issued_at: Utc::now(),
        run_id: fixture.run_id,
        harness_slug: fixture.harness_slug,
        turn_number: 2,
        expected_state_version: 2,
        operation: HarnessOperation::Fail {
            failure: object(json!({"code": "provider_rate_limit"})),
        },
    };
    let raw = serde_json::to_vec(&fail).unwrap();
    let result = apply_harness_command(&agent::Database::from_pool(app.pool.clone()), &fail, &raw)
        .await
        .unwrap();
    assert!(matches!(result.outcome, HarnessCommandOutcome::Applied(_)));
    let status: String = sqlx::query_scalar("select status from runs where run_id = $1")
        .bind(fixture.run_id)
        .fetch_one(&app.pool)
        .await
        .unwrap();
    assert_eq!(status, "failed");
}

#[tokio::test]
#[ignore = "requires AGENT_TEST_DATABASE_URL"]
async fn expired_wait_republishes_the_same_turn_without_an_agent_retry_attempt() {
    let app = test_app().await;
    let fixture = start_run(&app).await;
    let wait_id = Uuid::now_v7();
    let command = HarnessCommand {
        protocol_version: HARNESS_PROTOCOL_VERSION,
        command_id: Uuid::now_v7(),
        issued_at: Utc::now(),
        run_id: fixture.run_id,
        harness_slug: fixture.harness_slug,
        turn_number: 1,
        expected_state_version: 1,
        operation: HarnessOperation::Wait(WaitRequest {
            wait_id,
            harness_wait_id: "provider-backoff".to_owned(),
            kind: "retry".to_owned(),
            public_request: object(json!({})),
            resume_metadata: object(json!({"retry": 2})),
            expires_at: Some(Utc::now() + Duration::minutes(1)),
        }),
    };
    let raw = serde_json::to_vec(&command).unwrap();
    apply_harness_command(
        &agent::Database::from_pool(app.pool.clone()),
        &command,
        &raw,
    )
    .await
    .unwrap();
    sqlx::query("update run_waits set expires_at = now() - interval '1 second' where wait_id = $1")
        .bind(wait_id)
        .execute(&app.pool)
        .await
        .unwrap();
    assert_eq!(
        expire_waits_once(&agent::Database::from_pool(app.pool.clone()), 10)
            .await
            .unwrap(),
        1
    );

    let event = latest_turn(&app, fixture.run_id).await;
    assert_eq!(event.turn_number, 1);
    assert_eq!(event.expected_state_version, 3);
    assert_eq!(
        event.resume.unwrap().source,
        agent_contracts::WaitResolutionSource::Expired
    );
    let run: (String, i32) =
        sqlx::query_as("select status, current_turn from runs where run_id = $1")
            .bind(fixture.run_id)
            .fetch_one(&app.pool)
            .await
            .unwrap();
    assert_eq!(run, ("active".to_owned(), 1));
}

struct Fixture {
    run_id: Uuid,
    harness_slug: String,
}

async fn start_run(app: &TestApp) -> Fixture {
    let harness_id = unique("broker-harness");
    let harness_slug = unique("broker");
    let revision_id = unique("broker-revision");
    assert_eq!(app.control(Method::POST, "/v1/harnesses").json(&json!({"harness_id": harness_id, "slug": harness_slug, "display_name": "Broker test"})).send().await.unwrap().status(), StatusCode::CREATED);
    assert_eq!(app.control(Method::POST, &format!("/v1/harnesses/{harness_id}/revisions")).json(&json!({"harness_revision_id": revision_id, "revision": "v1", "contract_version": 1, "default_config": {}, "config_schema": null})).send().await.unwrap().status(), StatusCode::CREATED);
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
    let response = app.control(Method::POST, &format!("/v1/sessions/{session_id}/runs")).json(&json!({
        "run_id": run_id, "input": {"session_message_id": message_id, "message": user_message(message_id)},
        "harness": {"selection": "active_revision", "harness_id": harness_id}, "config_override": {},
        "limits": {"max_turns": 5}, "expected_session_revision": 0
    })).send().await.unwrap();
    assert_eq!(response.status(), StatusCode::ACCEPTED);
    Fixture {
        run_id,
        harness_slug,
    }
}

async fn latest_turn(app: &TestApp, run_id: Uuid) -> TurnRequested {
    let Json(payload): Json<Value> = sqlx::query_scalar("select payload from broker_outbox where payload->>'run_id' = $1 and subject like '%.turn.requested.v1' order by created_at desc limit 1")
        .bind(run_id.to_string()).fetch_one(&app.pool).await.unwrap();
    serde_json::from_value(payload).unwrap()
}
fn object(value: Value) -> llm_contracts::JsonObject {
    value.as_object().unwrap().clone()
}
fn user_message(id: Uuid) -> Value {
    json!({"role":"user","id":id.to_string(),"timestamp":1,"content":[{"type":"text","content":"start"}]})
}
