mod support;

use reqwest::{Method, StatusCode};
use serde_json::{Value, json};
use support::{assert_error, lazy_test_app, start_execution_fixture, test_app};
use uuid::Uuid;

#[tokio::test]
async fn steer_inspection_routes_enforce_authentication_and_validation() {
    let app = lazy_test_app().await;
    let run_id = Uuid::now_v7();

    let unauthorized = app
        .client
        .get(format!("{}/v1/runs/{run_id}/messages", app.base_url))
        .send()
        .await
        .expect("control auth response");
    assert_error(unauthorized, StatusCode::UNAUTHORIZED, "unauthorized").await;

    let invalid_limit = app
        .control(Method::GET, &format!("/v1/runs/{run_id}/messages?limit=0"))
        .send()
        .await
        .expect("invalid limit response");
    assert_error(invalid_limit, StatusCode::BAD_REQUEST, "invalid_request").await;

    let invalid_message_id = app
        .control(
            Method::GET,
            &format!("/v1/runs/{run_id}/messages/not-a-uuid"),
        )
        .send()
        .await
        .expect("invalid message id response");
    assert_error(
        invalid_message_id,
        StatusCode::BAD_REQUEST,
        "invalid_request",
    )
    .await;
}

#[tokio::test]
#[ignore = "requires AGENT_TEST_DATABASE_URL"]
async fn queued_messages_can_be_listed_filtered_and_read_after_discard() {
    let app = test_app().await;
    let fixture = start_execution_fixture(&app, 3, 3).await;
    let first_id = Uuid::now_v7();
    let second_id = Uuid::now_v7();

    for (message_id, content) in [(first_id, "first steer"), (second_id, "second steer")] {
        let response = app
            .control(
                Method::POST,
                &format!("/v1/runs/{}/messages", fixture.run_id),
            )
            .json(&json!({
                "expected_state_version": 1,
                "input": {
                    "session_message_id": message_id,
                    "message": user_message(message_id, content)
                }
            }))
            .send()
            .await
            .expect("queue message");
        assert_eq!(response.status(), StatusCode::ACCEPTED);
    }

    let first_page = get_json(
        &app,
        &format!("/v1/runs/{}/messages?state=pending&limit=1", fixture.run_id),
    )
    .await;
    assert_eq!(
        first_page["items"][0]["session_message_id"],
        first_id.to_string()
    );
    assert_eq!(first_page["items"][0]["queue_sequence"], 1);
    assert_eq!(first_page["next_after_sequence"], 1);

    let second_page = get_json(
        &app,
        &format!(
            "/v1/runs/{}/messages?after_sequence=1&limit=1",
            fixture.run_id
        ),
    )
    .await;
    assert_eq!(
        second_page["items"][0]["session_message_id"],
        second_id.to_string()
    );
    assert_eq!(second_page["items"][0]["queue_sequence"], 2);
    assert!(second_page["next_after_sequence"].is_null());

    let first = get_json(
        &app,
        &format!("/v1/runs/{}/messages/{first_id}", fixture.run_id),
    )
    .await;
    assert_eq!(first["state"], "pending");

    let abort_id = Uuid::now_v7();
    let aborted = app
        .control(Method::POST, &format!("/v1/runs/{}/abort", fixture.run_id))
        .json(&json!({
            "abort_id": abort_id,
            "expected_state_version": 1,
            "reason": "inspection test",
            "payload": {}
        }))
        .send()
        .await
        .expect("abort queued run");
    assert_eq!(aborted.status(), StatusCode::ACCEPTED);

    let discarded = get_json(
        &app,
        &format!("/v1/runs/{}/messages/{first_id}", fixture.run_id),
    )
    .await;
    assert_eq!(discarded["state"], "discarded");
    assert_eq!(discarded["discard_reason"], "run_aborted");

    let discarded_page = get_json(
        &app,
        &format!("/v1/runs/{}/messages?state=discarded", fixture.run_id),
    )
    .await;
    assert_eq!(discarded_page["items"].as_array().expect("items").len(), 2);
}

async fn get_json(app: &support::TestApp, path: &str) -> Value {
    let response = app
        .control(Method::GET, path)
        .send()
        .await
        .expect("inspection response");
    assert_eq!(response.status(), StatusCode::OK);
    response.json().await.expect("inspection body")
}

fn user_message(message_id: Uuid, content: &str) -> Value {
    json!({
        "role": "user",
        "id": message_id.to_string(),
        "timestamp": 1,
        "content": [{"type": "text", "content": content}]
    })
}
