mod support;

use agent::{
    Database,
    execution::{ExecutionPolicy, reap_expired_once},
};
use reqwest::{Method, StatusCode};
use serde_json::{Value, json};
use support::{assert_error, claim_execution, lazy_test_app, start_execution_fixture, test_app};
use uuid::Uuid;

const LEASE_HEADER: &str = "x-agent-lease-token";

#[tokio::test]
async fn abort_routes_enforce_authentication_and_validation() {
    let app = lazy_test_app().await;
    let run_id = Uuid::now_v7();
    let unauthorized = app
        .client
        .post(format!("{}/v1/runs/{run_id}/abort", app.base_url))
        .json(&json!({
            "abort_id": Uuid::now_v7(),
            "expected_state_version": 1,
            "reason": null
        }))
        .send()
        .await
        .expect("unauthorized abort response");
    assert_error(unauthorized, StatusCode::UNAUTHORIZED, "unauthorized").await;

    let invalid = app
        .control(Method::POST, &format!("/v1/runs/{run_id}/abort"))
        .json(&json!({
            "abort_id": Uuid::now_v7(),
            "expected_state_version": 0,
            "reason": " "
        }))
        .send()
        .await
        .expect("invalid abort response");
    assert_error(invalid, StatusCode::BAD_REQUEST, "invalid_request").await;
}

#[tokio::test]
#[ignore = "requires AGENT_TEST_DATABASE_URL"]
async fn cooperative_abort_is_delivered_acknowledged_and_resumed() {
    let app = test_app().await;
    let fixture = start_execution_fixture(&app, 3, 3).await;
    let (token, claim) = claim_execution(&app, &fixture, 31).await;
    let lease_version = claim["lease"]["lease_version"].as_u64().expect("lease");
    let state_version = claim["run"]["state_version"].as_u64().expect("state");
    let abort_id = Uuid::now_v7();
    let abort_command = json!({
        "abort_id": abort_id,
        "expected_state_version": state_version,
        "reason": "operator requested stop",
        "payload": {"ticket": "OPS-1"}
    });
    let aborted = app
        .control(Method::POST, &format!("/v1/runs/{}/abort", fixture.run_id))
        .json(&abort_command)
        .send()
        .await
        .expect("request abort");
    assert_eq!(aborted.status(), StatusCode::ACCEPTED);
    let aborted = aborted.json::<Value>().await.expect("abort result");
    assert_eq!(aborted["abort"]["status"], "pending");
    assert_eq!(aborted["run"]["status"], "aborting");
    assert_eq!(aborted["run"]["state_version"], state_version + 1);

    let heartbeat = app
        .worker(
            Method::POST,
            &format!("/v1/worker/runs/{}/heartbeat", fixture.run_id),
        )
        .header(LEASE_HEADER, &token)
        .json(&json!({"lease_version": lease_version}))
        .send()
        .await
        .expect("abort heartbeat");
    assert_eq!(heartbeat.status(), StatusCode::OK);
    let heartbeat = heartbeat.json::<Value>().await.expect("heartbeat body");
    assert_eq!(heartbeat["directives"][0]["type"], "abort");
    assert_eq!(
        heartbeat["directives"][0]["details"]["abort_id"],
        abort_id.to_string()
    );

    let abort_result_id = Uuid::now_v7();
    let appended = app
        .worker(
            Method::POST,
            &format!("/v1/worker/runs/{}/messages", fixture.run_id),
        )
        .header(LEASE_HEADER, &token)
        .json(&json!({
            "lease_version": lease_version,
            "expected_session_revision": 1,
            "messages": [{
                "session_message_id": abort_result_id,
                "message": {
                    "role": "tool_result",
                    "id": abort_result_id.to_string(),
                    "tool_name": "bash",
                    "tool_call_id": "call-aborted",
                    "content": [{"type": "text", "content": "Command aborted"}],
                    "timestamp": 1,
                    "outcome": {
                        "status": "error",
                        "error": {"message": "Command aborted", "name": "cancelled"}
                    }
                }
            }]
        }))
        .send()
        .await
        .expect("append aborted tool result");
    assert_eq!(appended.status(), StatusCode::CREATED);
    let appended = appended.json::<Value>().await.expect("appended result");
    assert_eq!(appended["items"][0]["revision"], 2);
    assert_eq!(
        appended["items"][0]["message"]["outcome"]["status"],
        "error"
    );

    let acknowledged = app
        .worker(
            Method::POST,
            &format!("/v1/worker/runs/{}/abort/acknowledge", fixture.run_id),
        )
        .header(LEASE_HEADER, &token)
        .json(&json!({
            "abort_id": abort_id,
            "lease_version": lease_version,
            "expected_state_version": state_version + 1,
            "resume_metadata": {"checkpoint": "safe"}
        }))
        .send()
        .await
        .expect("acknowledge abort");
    assert_eq!(acknowledged.status(), StatusCode::OK);
    let acknowledged = acknowledged.json::<Value>().await.expect("ack body");
    assert_eq!(acknowledged["abort"]["status"], "finalized");
    assert_eq!(acknowledged["abort"]["finalization_reason"], "acknowledged");
    assert_eq!(acknowledged["run"]["status"], "aborted");

    let resumed = app
        .control(Method::POST, &format!("/v1/runs/{}/resume", fixture.run_id))
        .json(&json!({
            "expected_state_version": state_version + 2,
            "resolution": {"continue": true}
        }))
        .send()
        .await
        .expect("resume run");
    assert_eq!(resumed.status(), StatusCode::ACCEPTED);
    let resumed = resumed.json::<Value>().await.expect("resume body");
    assert_eq!(resumed["abort"]["status"], "resumed");
    assert_eq!(resumed["run"]["status"], "queued");
    assert_eq!(resumed["run"]["current_turn"], 2);

    let (_, claim) = claim_execution(&app, &fixture, 32).await;
    assert_eq!(claim["resume"]["source"], "abort");
    assert_eq!(
        claim["resume"]["details"]["resume_metadata"],
        json!({"checkpoint": "safe"})
    );
    assert_eq!(
        claim["resume"]["details"]["resolution"],
        json!({"continue": true})
    );

    let listed = app
        .control(Method::GET, &format!("/v1/runs/{}/aborts", fixture.run_id))
        .send()
        .await
        .expect("list aborts")
        .json::<Value>()
        .await
        .expect("abort page");
    assert_eq!(listed["items"][0]["abort_id"], abort_id.to_string());
}

#[tokio::test]
#[ignore = "requires AGENT_TEST_DATABASE_URL"]
async fn abort_deadline_is_finalized_by_the_reaper() {
    let app = test_app().await;
    let fixture = start_execution_fixture(&app, 2, 2).await;
    let (_, claim) = claim_execution(&app, &fixture, 41).await;
    let state_version = claim["run"]["state_version"].as_u64().expect("state");
    let abort_id = Uuid::now_v7();
    let response = app
        .control(Method::POST, &format!("/v1/runs/{}/abort", fixture.run_id))
        .json(&json!({
            "abort_id": abort_id,
            "expected_state_version": state_version,
            "reason": null,
            "payload": {}
        }))
        .send()
        .await
        .expect("request abort");
    assert_eq!(response.status(), StatusCode::ACCEPTED);
    sqlx::query(
        "update run_aborts set requested_at = now() - interval '2 seconds', \
         deadline_at = now() - interval '1 second' where abort_id = $1",
    )
    .bind(abort_id)
    .execute(&app.pool)
    .await
    .expect("expire abort deadline");
    sqlx::query(
        "update run_leases set acquired_at = now() - interval '2 seconds', \
         expires_at = now() - interval '1 second' where run_id = $1",
    )
    .bind(fixture.run_id)
    .execute(&app.pool)
    .await
    .expect("expire lease");
    let count = reap_expired_once(
        &Database::from_pool(app.pool.clone()),
        ExecutionPolicy::default(),
    )
    .await
    .expect("reap abort");
    assert_eq!(count, 1);

    let abort = app
        .control(Method::GET, &format!("/v1/aborts/{abort_id}"))
        .send()
        .await
        .expect("get abort")
        .json::<Value>()
        .await
        .expect("abort body");
    assert_eq!(abort["status"], "finalized");
    assert_eq!(abort["finalization_reason"], "deadline_expired");
}

#[tokio::test]
#[ignore = "requires AGENT_TEST_DATABASE_URL"]
async fn unowned_and_waiting_runs_abort_immediately() {
    let app = test_app().await;
    let queued = start_execution_fixture(&app, 3, 3).await;
    let queued_abort_id = Uuid::now_v7();
    let response = app
        .control(Method::POST, &format!("/v1/runs/{}/abort", queued.run_id))
        .json(&json!({
            "abort_id": queued_abort_id,
            "expected_state_version": 1,
            "reason": null,
            "payload": {}
        }))
        .send()
        .await
        .expect("abort queued run");
    assert_eq!(response.status(), StatusCode::ACCEPTED);
    let response = response.json::<Value>().await.expect("queued abort body");
    assert_eq!(response["abort"]["status"], "finalized");
    assert_eq!(response["abort"]["finalization_reason"], "no_worker");
    assert_eq!(response["run"]["status"], "aborted");

    let waiting = start_execution_fixture(&app, 3, 3).await;
    let (token, claim) = claim_execution(&app, &waiting, 51).await;
    let wait_id = Uuid::now_v7();
    let wait = app
        .worker(
            Method::POST,
            &format!("/v1/worker/runs/{}/wait", waiting.run_id),
        )
        .header(LEASE_HEADER, token)
        .json(&json!({
            "lease_version": claim["lease"]["lease_version"],
            "expected_state_version": claim["run"]["state_version"],
            "wait_id": wait_id,
            "harness_wait_id": "approval-before-abort",
            "kind": "approval",
            "public_request": {},
            "resume_metadata": {"checkpoint": "waiting"}
        }))
        .send()
        .await
        .expect("request wait");
    assert_eq!(wait.status(), StatusCode::ACCEPTED);

    let abort_id = Uuid::now_v7();
    let response = app
        .control(Method::POST, &format!("/v1/runs/{}/abort", waiting.run_id))
        .json(&json!({
            "abort_id": abort_id,
            "expected_state_version": 3,
            "reason": "cancel approval",
            "payload": {}
        }))
        .send()
        .await
        .expect("abort waiting run");
    assert_eq!(response.status(), StatusCode::ACCEPTED);
    let response = response.json::<Value>().await.expect("waiting abort body");
    assert_eq!(response["abort"]["status"], "finalized");
    assert_eq!(
        response["abort"]["resume_metadata"],
        json!({"checkpoint": "waiting"})
    );
    let wait = app
        .control(Method::GET, &format!("/v1/waits/{wait_id}"))
        .send()
        .await
        .expect("get cancelled wait")
        .json::<Value>()
        .await
        .expect("cancelled wait body");
    assert_eq!(wait["status"], "cancelled");
}
