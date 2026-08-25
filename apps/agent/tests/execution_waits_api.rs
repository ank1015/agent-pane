mod support;

use reqwest::{Method, StatusCode};
use serde_json::{Value, json};
use support::{assert_error, claim_execution, lazy_test_app, start_execution_fixture, test_app};
use uuid::Uuid;

const LEASE_HEADER: &str = "x-agent-lease-token";

#[tokio::test]
async fn wait_routes_enforce_authentication_and_validation() {
    let app = lazy_test_app().await;
    let run_id = Uuid::now_v7();
    let wait_id = Uuid::now_v7();
    let unauthorized = app
        .client
        .get(format!("{}/v1/waits/{wait_id}", app.base_url))
        .send()
        .await
        .expect("unauthorized wait response");
    assert_error(unauthorized, StatusCode::UNAUTHORIZED, "unauthorized").await;

    let invalid = app
        .worker(Method::POST, &format!("/v1/worker/runs/{run_id}/wait"))
        .header(LEASE_HEADER, support::lease_token(1))
        .json(&json!({
            "lease_version": 0,
            "expected_state_version": 0,
            "wait_id": wait_id,
            "harness_wait_id": "",
            "kind": ""
        }))
        .send()
        .await
        .expect("invalid wait response");
    assert_error(invalid, StatusCode::BAD_REQUEST, "invalid_request").await;
}

#[tokio::test]
#[ignore = "requires AGENT_TEST_DATABASE_URL"]
async fn worker_waits_and_resumes_with_resolution_context() {
    let app = test_app().await;
    let fixture = start_execution_fixture(&app, 3, 3).await;
    let (token, claim) = claim_execution(&app, &fixture, 21).await;
    let lease_version = claim["lease"]["lease_version"]
        .as_u64()
        .expect("lease version");
    let state_version = claim["run"]["state_version"]
        .as_u64()
        .expect("state version");
    let wait_id = Uuid::now_v7();
    let command = json!({
        "lease_version": lease_version,
        "expected_state_version": state_version,
        "wait_id": wait_id,
        "harness_wait_id": "approval-1",
        "kind": "approval",
        "public_request": {"prompt": "Deploy?"},
        "resume_metadata": {"step": "deploy"}
    });

    let requested = app
        .worker(
            Method::POST,
            &format!("/v1/worker/runs/{}/wait", fixture.run_id),
        )
        .header(LEASE_HEADER, &token)
        .json(&command)
        .send()
        .await
        .expect("request wait");
    assert_eq!(requested.status(), StatusCode::ACCEPTED);
    let requested = requested.json::<Value>().await.expect("wait body");
    assert_eq!(requested["wait"]["status"], "pending");
    assert_eq!(requested["run"]["status"], "waiting");
    assert_eq!(requested["run"]["state_version"], state_version + 1);

    let replay = app
        .worker(
            Method::POST,
            &format!("/v1/worker/runs/{}/wait", fixture.run_id),
        )
        .header(LEASE_HEADER, &token)
        .json(&command)
        .send()
        .await
        .expect("replay wait");
    assert_eq!(replay.status(), StatusCode::OK);
    assert_eq!(
        replay.json::<Value>().await.expect("replay body"),
        requested
    );

    let pending = app
        .control(Method::GET, "/v1/waits?status=pending&limit=1")
        .send()
        .await
        .expect("list waits")
        .json::<Value>()
        .await
        .expect("wait page");
    assert_eq!(pending["items"][0]["wait_id"], wait_id.to_string());
    let run_waits = app
        .control(Method::GET, &format!("/v1/runs/{}/waits", fixture.run_id))
        .send()
        .await
        .expect("list run waits")
        .json::<Value>()
        .await
        .expect("run wait page");
    assert_eq!(run_waits["items"][0]["kind"], "approval");

    let resolution = json!({
        "expected_state_version": state_version + 1,
        "resolution": {"approved": true}
    });
    let resolved = app
        .control(Method::POST, &format!("/v1/waits/{wait_id}/resolve"))
        .json(&resolution)
        .send()
        .await
        .expect("resolve wait");
    assert_eq!(resolved.status(), StatusCode::OK);
    let resolved = resolved.json::<Value>().await.expect("resolved body");
    assert_eq!(resolved["wait"]["status"], "resolved");
    assert_eq!(resolved["run"]["status"], "queued");
    assert_eq!(resolved["run"]["state_version"], state_version + 2);

    let (_, resumed_claim) = claim_execution(&app, &fixture, 22).await;
    assert_eq!(resumed_claim["run"]["current_turn"], 1);
    assert_eq!(resumed_claim["resume"]["source"], "wait");
    assert_eq!(
        resumed_claim["resume"]["details"]["resolution"],
        json!({"approved": true})
    );
    assert_eq!(
        resumed_claim["resume"]["details"]["resume_metadata"],
        json!({"step": "deploy"})
    );
}
