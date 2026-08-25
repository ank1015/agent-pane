mod support;

use agent::{
    Database,
    execution::{ExecutionPolicy, reap_expired_once},
};
use base64::{Engine as _, engine::general_purpose::URL_SAFE_NO_PAD};
use reqwest::{Method, StatusCode, header::CACHE_CONTROL};
use serde_json::{Value, json};
use support::{assert_error, lazy_test_app, test_app, unique};
use uuid::Uuid;

const LEASE_HEADER: &str = "x-agent-lease-token";

#[tokio::test]
async fn worker_routes_require_worker_and_lease_authentication() {
    let app = lazy_test_app().await;
    let run_id = Uuid::now_v7();
    let claim = claim_command(Uuid::now_v7(), "revision-1");

    let unauthorized = app
        .client
        .post(format!("{}/v1/worker/runs/claim", app.base_url))
        .json(&claim)
        .send()
        .await
        .expect("worker auth response");
    assert_error(
        unauthorized,
        StatusCode::UNAUTHORIZED,
        "unauthorized_worker",
    )
    .await;

    let missing_lease = app
        .worker(Method::POST, "/v1/worker/runs/claim")
        .json(&claim)
        .send()
        .await
        .expect("lease auth response");
    assert_error(missing_lease, StatusCode::UNAUTHORIZED, "missing_run_lease").await;

    let invalid_path = app
        .worker(Method::POST, &format!("/v1/worker/runs/{run_id}/heartbeat"))
        .header(LEASE_HEADER, "not-base64")
        .json(&json!({"lease_version": 1}))
        .send()
        .await
        .expect("invalid lease response");
    assert_error(invalid_path, StatusCode::UNAUTHORIZED, "invalid_run_lease").await;
}

#[tokio::test]
#[ignore = "requires AGENT_TEST_DATABASE_URL"]
async fn worker_can_claim_append_steer_continue_and_complete() {
    let app = test_app().await;
    let fixture = start_fixture(&app, 3, 3).await;
    let first_token = lease_token(7);
    let first_lease_id = Uuid::now_v7();
    let claim = claim_command(first_lease_id, &fixture.revision_id);

    let claimed = app
        .worker(Method::POST, "/v1/worker/runs/claim")
        .header(LEASE_HEADER, &first_token)
        .json(&claim)
        .send()
        .await
        .expect("claim run");
    assert_eq!(claimed.status(), StatusCode::OK);
    assert_eq!(claimed.headers()[CACHE_CONTROL], "no-store");
    let claimed = claimed.json::<Value>().await.expect("claimed run body");
    assert_eq!(claimed["run"]["run_id"], fixture.run_id.to_string());
    assert_eq!(claimed["run"]["status"], "running");
    assert_eq!(claimed["run"]["state_version"], 2);
    assert_eq!(claimed["lease"]["lease_version"], 2);

    let replay = app
        .worker(Method::POST, "/v1/worker/runs/claim")
        .header(LEASE_HEADER, &first_token)
        .json(&claim)
        .send()
        .await
        .expect("replay claim");
    assert_eq!(replay.status(), StatusCode::OK);
    assert_eq!(
        replay.json::<Value>().await.expect("replayed claim"),
        claimed
    );

    let heartbeat = app
        .worker(
            Method::POST,
            &format!("/v1/worker/runs/{}/heartbeat", fixture.run_id),
        )
        .header(LEASE_HEADER, &first_token)
        .json(&json!({"lease_version": 2}))
        .send()
        .await
        .expect("heartbeat");
    assert_eq!(heartbeat.status(), StatusCode::OK);
    assert_eq!(
        heartbeat.json::<Value>().await.expect("heartbeat body")["state_version"],
        2
    );

    let transcript = worker_messages(&app, fixture.run_id, 2, &first_token, 0).await;
    assert_eq!(transcript["items"].as_array().expect("items").len(), 1);

    let first_output_id = Uuid::now_v7();
    let appended = append_messages(
        &app,
        fixture.run_id,
        2,
        1,
        &first_token,
        first_output_id,
        "turn one output",
    )
    .await;
    assert_eq!(appended["current_session_revision"], 2);
    assert_eq!(appended["items"][0]["revision"], 2);

    let steer_id = Uuid::now_v7();
    let steer_command = json!({
        "expected_state_version": 2,
        "input": {
            "session_message_id": steer_id,
            "message": user_message(steer_id, "change direction")
        }
    });
    let steered = app
        .control(
            Method::POST,
            &format!("/v1/runs/{}/messages", fixture.run_id),
        )
        .json(&steer_command)
        .send()
        .await
        .expect("queue steer");
    assert_eq!(steered.status(), StatusCode::ACCEPTED);
    let steered = steered.json::<Value>().await.expect("steer body");
    assert_eq!(steered["state"], "pending");
    assert_eq!(steered["queued_during_turn"], 1);

    let continued = app
        .worker(
            Method::POST,
            &format!("/v1/worker/runs/{}/complete", fixture.run_id),
        )
        .header(LEASE_HEADER, &first_token)
        .json(&json!({
            "lease_version": 2,
            "expected_state_version": 2,
            "disposition": "continue",
            "final_message_id": null
        }))
        .send()
        .await
        .expect("continue run");
    assert_eq!(continued.status(), StatusCode::OK);
    let continued = continued.json::<Value>().await.expect("continued body");
    assert_eq!(continued["run"]["status"], "queued");
    assert_eq!(continued["run"]["current_turn"], 2);
    assert_eq!(continued["run"]["state_version"], 3);
    assert_eq!(continued["committed_messages"][0]["revision"], 3);

    let inspected_steer = app
        .control(
            Method::GET,
            &format!("/v1/runs/{}/messages/{steer_id}", fixture.run_id),
        )
        .send()
        .await
        .expect("inspect committed steer");
    assert_eq!(inspected_steer.status(), StatusCode::OK);
    let inspected_steer = inspected_steer
        .json::<Value>()
        .await
        .expect("committed steer body");
    assert_eq!(inspected_steer["state"], "committed");
    assert_eq!(inspected_steer["revision"], 3);

    let committed_steers = app
        .control(
            Method::GET,
            &format!("/v1/runs/{}/messages?state=committed", fixture.run_id),
        )
        .send()
        .await
        .expect("list committed steers")
        .json::<Value>()
        .await
        .expect("committed steer page");
    assert_eq!(
        committed_steers["items"]
            .as_array()
            .expect("committed steer items")
            .len(),
        1
    );

    let steer_replay = app
        .control(
            Method::POST,
            &format!("/v1/runs/{}/messages", fixture.run_id),
        )
        .json(&steer_command)
        .send()
        .await
        .expect("replay applied steer");
    assert_eq!(steer_replay.status(), StatusCode::OK);
    assert_eq!(
        steer_replay.json::<Value>().await.expect("steer replay")["state"],
        "committed"
    );

    let second_token = lease_token(8);
    let second_claim = app
        .worker(Method::POST, "/v1/worker/runs/claim")
        .header(LEASE_HEADER, &second_token)
        .json(&claim_command(Uuid::now_v7(), &fixture.revision_id))
        .send()
        .await
        .expect("claim second turn")
        .json::<Value>()
        .await
        .expect("second claim body");
    assert_eq!(second_claim["run"]["state_version"], 4);
    assert_eq!(second_claim["current_session_revision"], 3);

    let second_page = worker_messages(&app, fixture.run_id, 4, &second_token, 1).await;
    assert_eq!(second_page["items"].as_array().expect("items").len(), 2);
    let final_id = Uuid::now_v7();
    append_messages(
        &app,
        fixture.run_id,
        4,
        3,
        &second_token,
        final_id,
        "final output",
    )
    .await;

    let completed = app
        .worker(
            Method::POST,
            &format!("/v1/worker/runs/{}/complete", fixture.run_id),
        )
        .header(LEASE_HEADER, &second_token)
        .json(&json!({
            "lease_version": 4,
            "expected_state_version": 4,
            "disposition": "complete",
            "final_message_id": final_id
        }))
        .send()
        .await
        .expect("complete run");
    assert_eq!(completed.status(), StatusCode::OK);
    let completed = completed.json::<Value>().await.expect("completed body");
    assert_eq!(completed["run"]["status"], "completed");
    assert_eq!(completed["run"]["state_version"], 5);
    assert_eq!(completed["run"]["final_message_id"], final_id.to_string());
}

#[tokio::test]
#[ignore = "requires AGENT_TEST_DATABASE_URL"]
async fn explicit_failure_and_reaping_share_the_retry_budget() {
    let app = test_app().await;
    let fixture = start_fixture(&app, 2, 2).await;
    let first_token = lease_token(11);
    claim_run(&app, &fixture, &first_token).await;

    let failed = app
        .worker(
            Method::POST,
            &format!("/v1/worker/runs/{}/fail", fixture.run_id),
        )
        .header(LEASE_HEADER, &first_token)
        .json(&json!({
            "lease_version": 2,
            "expected_state_version": 2,
            "failure": {"code": "provider_error"}
        }))
        .send()
        .await
        .expect("fail turn")
        .json::<Value>()
        .await
        .expect("failed body");
    assert_eq!(failed["will_retry"], true);
    assert_eq!(failed["run"]["status"], "queued");
    assert_eq!(failed["run"]["failures_in_current_turn"], 1);

    let second_token = lease_token(12);
    let claim = claim_run(&app, &fixture, &second_token).await;
    assert_eq!(claim["run"]["state_version"], 4);
    let steer_id = Uuid::now_v7();
    let queued = app
        .control(
            Method::POST,
            &format!("/v1/runs/{}/messages", fixture.run_id),
        )
        .json(&json!({
            "expected_state_version": 4,
            "input": {
                "session_message_id": steer_id,
                "message": user_message(steer_id, "pending steer")
            }
        }))
        .send()
        .await
        .expect("queue steer");
    assert_eq!(queued.status(), StatusCode::ACCEPTED);

    sqlx::query(
        "update run_leases set acquired_at = now() - interval '2 seconds', \
         expires_at = now() - interval '1 second' where run_id = $1",
    )
    .bind(fixture.run_id)
    .execute(&app.pool)
    .await
    .expect("expire lease");
    let database = Database::from_pool(app.pool.clone());
    assert_eq!(
        reap_expired_once(&database, ExecutionPolicy::default())
            .await
            .expect("reap lease"),
        1
    );

    let run = app
        .control(Method::GET, &format!("/v1/runs/{}", fixture.run_id))
        .send()
        .await
        .expect("read reaped run")
        .json::<Value>()
        .await
        .expect("reaped run body");
    assert_eq!(run["status"], "failed");
    assert_eq!(run["failures_in_current_turn"], 2);
    assert_eq!(run["failure"]["code"], "lease_expired");
    let steer_state: String =
        sqlx::query_scalar("select state from session_messages where session_message_id = $1")
            .bind(steer_id)
            .fetch_one(&app.pool)
            .await
            .expect("steer state");
    assert_eq!(steer_state, "discarded");
}

struct Fixture {
    run_id: Uuid,
    revision_id: String,
}

async fn start_fixture(app: &support::TestApp, max_turns: u32, max_failures: u32) -> Fixture {
    let harness_id = unique("worker-harness");
    let revision_id = unique("worker-revision");
    create_harness_revision(app, &harness_id, &revision_id).await;
    let session_id = Uuid::now_v7();
    let created = app
        .control(Method::POST, "/v1/sessions")
        .json(&json!({"session_id": session_id}))
        .send()
        .await
        .expect("create session");
    assert_eq!(created.status(), StatusCode::CREATED);
    let run_id = Uuid::now_v7();
    let input_id = Uuid::now_v7();
    let started = app
        .control(Method::POST, &format!("/v1/sessions/{session_id}/runs"))
        .json(&json!({
            "run_id": run_id,
            "input": {
                "session_message_id": input_id,
                "message": user_message(input_id, "start")
            },
            "harness": {"selection": "active_revision", "harness_id": harness_id},
            "config_override": {},
            "limits": {
                "max_turns": max_turns,
                "max_failures_per_turn": max_failures
            },
            "expected_session_revision": 0
        }))
        .send()
        .await
        .expect("start run");
    assert_eq!(started.status(), StatusCode::ACCEPTED);
    Fixture {
        run_id,
        revision_id,
    }
}

async fn create_harness_revision(app: &support::TestApp, harness_id: &str, revision_id: &str) {
    let created = app
        .control(Method::POST, "/v1/harnesses")
        .json(&json!({
            "harness_id": harness_id,
            "slug": unique("worker"),
            "display_name": "Worker lifecycle test"
        }))
        .send()
        .await
        .expect("create harness");
    assert_eq!(created.status(), StatusCode::CREATED);
    let revision = app
        .control(
            Method::POST,
            &format!("/v1/harnesses/{harness_id}/revisions"),
        )
        .json(&json!({
            "harness_revision_id": revision_id,
            "revision": "v1",
            "contract_version": 1,
            "default_config": {},
            "config_schema": null
        }))
        .send()
        .await
        .expect("create revision");
    assert_eq!(revision.status(), StatusCode::CREATED);
    let active = app
        .control(
            Method::PUT,
            &format!("/v1/harnesses/{harness_id}/active-revision"),
        )
        .json(&json!({"harness_revision_id": revision_id}))
        .send()
        .await
        .expect("activate revision");
    assert_eq!(active.status(), StatusCode::OK);
    let enabled = app
        .control(Method::PUT, &format!("/v1/harnesses/{harness_id}/enabled"))
        .json(&json!({"enabled": true}))
        .send()
        .await
        .expect("enable harness");
    assert_eq!(enabled.status(), StatusCode::OK);
}

async fn claim_run(app: &support::TestApp, fixture: &Fixture, token: &str) -> Value {
    app.worker(Method::POST, "/v1/worker/runs/claim")
        .header(LEASE_HEADER, token)
        .json(&claim_command(Uuid::now_v7(), &fixture.revision_id))
        .send()
        .await
        .expect("claim run")
        .json::<Value>()
        .await
        .expect("claim body")
}

async fn worker_messages(
    app: &support::TestApp,
    run_id: Uuid,
    lease_version: u64,
    token: &str,
    after_revision: u64,
) -> Value {
    app.worker(
        Method::GET,
        &format!(
            "/v1/worker/runs/{run_id}/messages?lease_version={lease_version}&after_revision={after_revision}"
        ),
    )
    .header(LEASE_HEADER, token)
    .send()
    .await
    .expect("read messages")
    .json::<Value>()
    .await
    .expect("messages body")
}

async fn append_messages(
    app: &support::TestApp,
    run_id: Uuid,
    lease_version: u64,
    expected_revision: u64,
    token: &str,
    message_id: Uuid,
    content: &str,
) -> Value {
    app.worker(Method::POST, &format!("/v1/worker/runs/{run_id}/messages"))
        .header(LEASE_HEADER, token)
        .json(&json!({
            "lease_version": lease_version,
            "expected_session_revision": expected_revision,
            "messages": [{
                "session_message_id": message_id,
                "message": custom_message(message_id, content)
            }]
        }))
        .send()
        .await
        .expect("append messages")
        .json::<Value>()
        .await
        .expect("append body")
}

fn claim_command(lease_id: Uuid, revision_id: &str) -> Value {
    json!({
        "lease_id": lease_id,
        "worker_instance_id": "worker-1",
        "supported_harness_revision_ids": [revision_id]
    })
}

fn lease_token(byte: u8) -> String {
    URL_SAFE_NO_PAD.encode([byte; 32])
}

fn user_message(message_id: Uuid, content: &str) -> Value {
    json!({
        "role": "user",
        "id": message_id.to_string(),
        "timestamp": 1,
        "content": [{"type": "text", "content": content}]
    })
}

fn custom_message(message_id: Uuid, content: &str) -> Value {
    json!({
        "role": "custom",
        "id": message_id.to_string(),
        "timestamp": 1,
        "content": {"text": content}
    })
}
