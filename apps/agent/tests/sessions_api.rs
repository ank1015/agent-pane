mod support;

use reqwest::{Method, StatusCode, header::CACHE_CONTROL};
use serde_json::{Value, json};
use support::{assert_error, test_app, unique};
use uuid::Uuid;

#[tokio::test]
#[ignore = "requires AGENT_TEST_DATABASE_URL"]
async fn sessions_can_be_created_retried_and_read() {
    let app = test_app().await;
    let session_id = Uuid::now_v7();

    let unauthorized = app
        .client
        .post(format!("{}/v1/sessions", app.base_url))
        .json(&json!({ "session_id": session_id }))
        .send()
        .await
        .expect("unauthorized request");
    assert_error(unauthorized, StatusCode::UNAUTHORIZED, "unauthorized").await;

    let created = app
        .control(Method::POST, "/v1/sessions")
        .json(&json!({ "session_id": session_id }))
        .send()
        .await
        .expect("create session");
    assert_eq!(created.status(), StatusCode::CREATED);
    assert_eq!(created.headers()[CACHE_CONTROL], "no-store");
    let first = created.json::<Value>().await.expect("created session");
    assert_eq!(first["session_id"], session_id.to_string());
    assert_eq!(first["current_revision"], 0);
    assert!(first.get("archived_at").is_none());

    let retried = app
        .control(Method::POST, "/v1/sessions")
        .json(&json!({ "session_id": session_id }))
        .send()
        .await
        .expect("retry session");
    assert_eq!(retried.status(), StatusCode::OK);
    assert_eq!(retried.json::<Value>().await.expect("retried body"), first);

    let fetched = app
        .control(Method::GET, &format!("/v1/sessions/{session_id}"))
        .send()
        .await
        .expect("get session");
    assert_eq!(fetched.status(), StatusCode::OK);
    assert_eq!(fetched.json::<Value>().await.expect("fetched body"), first);

    let invalid_id = app
        .control(Method::GET, "/v1/sessions/not-a-uuid")
        .send()
        .await
        .expect("invalid id");
    assert_error(invalid_id, StatusCode::BAD_REQUEST, "invalid_request").await;

    let missing = app
        .control(Method::GET, &format!("/v1/sessions/{}", Uuid::now_v7()))
        .send()
        .await
        .expect("missing session");
    assert_error(missing, StatusCode::NOT_FOUND, "session_not_found").await;

    let unknown_field = app
        .control(Method::POST, "/v1/sessions")
        .json(&json!({ "session_id": Uuid::now_v7(), "extra": true }))
        .send()
        .await
        .expect("unknown field");
    assert_error(unknown_field, StatusCode::BAD_REQUEST, "invalid_request").await;
}

#[tokio::test]
#[ignore = "requires AGENT_TEST_DATABASE_URL"]
async fn session_transcript_and_run_history_are_paginated() {
    let app = test_app().await;
    let session_id = Uuid::now_v7();
    create_session(&app, session_id).await;

    let harness_id = unique("session-test-harness");
    let revision_id = unique("session-test-revision");
    sqlx::query("INSERT INTO harnesses (harness_id, slug, display_name) VALUES ($1, $2, $3)")
        .bind(&harness_id)
        .bind(unique("session-test"))
        .bind("Session test")
        .execute(&app.pool)
        .await
        .expect("harness fixture");
    sqlx::query(
        "INSERT INTO harness_revisions \
         (harness_revision_id, harness_id, revision, contract_version) VALUES ($1, $2, $3, 1)",
    )
    .bind(&revision_id)
    .bind(&harness_id)
    .bind(unique("revision"))
    .execute(&app.pool)
    .await
    .expect("revision fixture");

    let message_ids = [Uuid::now_v7(), Uuid::now_v7(), Uuid::now_v7()];
    for (index, message_id) in message_ids.iter().enumerate() {
        let revision = i64::try_from(index + 1).expect("revision");
        sqlx::query(
            "INSERT INTO session_messages \
             (session_message_id, session_id, revision, message, origin, delivery, state, committed_at) \
             VALUES ($1, $2, $3, $4, 'external', 'immediate', 'committed', now())",
        )
        .bind(message_id)
        .bind(session_id)
        .bind(revision)
        .bind(user_message(*message_id, &format!("message {revision}")))
        .execute(&app.pool)
        .await
        .expect("message fixture");
    }
    sqlx::query("UPDATE sessions SET current_revision = 3 WHERE session_id = $1")
        .bind(session_id)
        .execute(&app.pool)
        .await
        .expect("session revision fixture");

    let completed_run = Uuid::now_v7();
    sqlx::query(
        "INSERT INTO runs \
         (run_id, session_id, trigger_message_id, harness_revision_id, status, max_turns, \
          final_message_id, finished_at) \
         VALUES ($1, $2, $3, $4, 'completed', 5, $3, now())",
    )
    .bind(completed_run)
    .bind(session_id)
    .bind(message_ids[0])
    .bind(&revision_id)
    .execute(&app.pool)
    .await
    .expect("completed run fixture");

    let active_run = Uuid::now_v7();
    sqlx::query(
        "INSERT INTO runs \
         (run_id, session_id, trigger_message_id, harness_revision_id, max_turns) \
         VALUES ($1, $2, $3, $4, 5)",
    )
    .bind(active_run)
    .bind(session_id)
    .bind(message_ids[1])
    .bind(&revision_id)
    .execute(&app.pool)
    .await
    .expect("queued run fixture");

    let pending_id = Uuid::now_v7();
    sqlx::query(
        "INSERT INTO session_messages \
         (session_message_id, session_id, message, origin, delivery, state, run_id, \
          queued_during_turn, queue_sequence) \
         VALUES ($1, $2, $3, 'external', 'next_turn', 'pending', $4, 1, 1)",
    )
    .bind(pending_id)
    .bind(session_id)
    .bind(user_message(pending_id, "pending steer"))
    .bind(active_run)
    .execute(&app.pool)
    .await
    .expect("pending message fixture");

    let first_messages =
        get_json(&app, &format!("/v1/sessions/{session_id}/messages?limit=2")).await;
    assert_eq!(first_messages["items"].as_array().expect("items").len(), 2);
    assert_eq!(first_messages["next_after_revision"], 2);
    let second_messages = get_json(
        &app,
        &format!("/v1/sessions/{session_id}/messages?after_revision=2&limit=2"),
    )
    .await;
    assert_eq!(second_messages["items"].as_array().expect("items").len(), 1);
    assert!(second_messages["next_after_revision"].is_null());
    assert!(
        first_messages["items"]
            .as_array()
            .expect("items")
            .iter()
            .chain(second_messages["items"].as_array().expect("items"))
            .all(|item| item["session_message_id"] != pending_id.to_string())
    );

    let first_runs = get_json(&app, &format!("/v1/sessions/{session_id}/runs?limit=1")).await;
    assert_eq!(first_runs["items"].as_array().expect("items").len(), 1);
    let cursor = first_runs["next_cursor"].as_str().expect("next cursor");
    let second_runs = get_json(
        &app,
        &format!("/v1/sessions/{session_id}/runs?limit=1&cursor={cursor}"),
    )
    .await;
    assert_eq!(second_runs["items"].as_array().expect("items").len(), 1);
    assert!(second_runs["next_cursor"].is_null());

    let active = get_json(
        &app,
        &format!("/v1/sessions/{session_id}/runs?status=active"),
    )
    .await;
    assert_eq!(active["items"].as_array().expect("items").len(), 1);
    assert_eq!(active["items"][0]["run_id"], active_run.to_string());

    let invalid_limit = app
        .control(
            Method::GET,
            &format!("/v1/sessions/{session_id}/messages?limit=0"),
        )
        .send()
        .await
        .expect("invalid limit");
    assert_error(invalid_limit, StatusCode::BAD_REQUEST, "invalid_request").await;
}

async fn create_session(app: &support::TestApp, session_id: Uuid) {
    let response = app
        .control(Method::POST, "/v1/sessions")
        .json(&json!({ "session_id": session_id }))
        .send()
        .await
        .expect("create session");
    assert_eq!(response.status(), StatusCode::CREATED);
}

async fn get_json(app: &support::TestApp, path: &str) -> Value {
    let response = app
        .control(Method::GET, path)
        .send()
        .await
        .expect("get request");
    assert_eq!(response.status(), StatusCode::OK);
    response.json().await.expect("json response")
}

fn user_message(message_id: Uuid, content: &str) -> Value {
    json!({
        "role": "user",
        "id": message_id.to_string(),
        "timestamp": 1,
        "content": [{ "type": "text", "content": content }]
    })
}
