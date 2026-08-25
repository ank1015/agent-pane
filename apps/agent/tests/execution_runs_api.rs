mod support;

use reqwest::{Method, StatusCode, header::CACHE_CONTROL};
use serde_json::{Value, json};
use support::{assert_error, lazy_test_app, test_app, unique};
use uuid::Uuid;

#[tokio::test]
async fn run_routes_require_control_authentication_and_validate_input() {
    let app = lazy_test_app().await;
    let session_id = Uuid::now_v7();

    let unauthorized = app
        .client
        .post(format!("{}/v1/sessions/{session_id}/runs", app.base_url))
        .json(&start_command(Uuid::now_v7(), Uuid::now_v7(), "harness-1"))
        .send()
        .await
        .expect("unauthorized response");
    assert_error(unauthorized, StatusCode::UNAUTHORIZED, "unauthorized").await;

    let mut invalid = start_command(Uuid::now_v7(), Uuid::now_v7(), "harness-1");
    invalid["limits"]["max_turns"] = json!(0);
    let invalid = app
        .control(Method::POST, &format!("/v1/sessions/{session_id}/runs"))
        .json(&invalid)
        .send()
        .await
        .expect("validation response");
    let body = assert_error(invalid, StatusCode::BAD_REQUEST, "invalid_request").await;
    assert!(body["error"]["details"]["issues"].is_array());

    let invalid_path = app
        .control(Method::GET, "/v1/runs/not-a-uuid")
        .send()
        .await
        .expect("invalid path response");
    assert_error(invalid_path, StatusCode::BAD_REQUEST, "invalid_request").await;
}

#[tokio::test]
#[ignore = "requires AGENT_TEST_DATABASE_URL"]
async fn starts_reads_and_safely_retries_a_run() {
    let app = test_app().await;
    let harness_id = unique("execution-harness");
    let revision_id = unique("execution-revision");
    create_harness_revision(&app, &harness_id, &revision_id).await;
    let session_id = create_session(&app).await;
    let run_id = Uuid::now_v7();
    let message_id = Uuid::now_v7();
    let mut command = start_command(run_id, message_id, &harness_id);
    command["config_override"] = json!({
        "sampling": {"temperature": 0.2},
        "removed": null
    });
    command["limits"] = json!({
        "max_turns": 12,
        "max_failures_per_turn": 2
    });

    let created = app
        .control(Method::POST, &format!("/v1/sessions/{session_id}/runs"))
        .json(&command)
        .send()
        .await
        .expect("start run");
    assert_eq!(created.status(), StatusCode::ACCEPTED);
    assert_eq!(created.headers()[CACHE_CONTROL], "no-store");
    let accepted = created.json::<Value>().await.expect("accepted run");
    assert_eq!(
        accepted["trigger_message"]["session_message_id"],
        message_id.to_string()
    );
    assert_eq!(accepted["trigger_message"]["revision"], 1);
    assert_eq!(accepted["trigger_message"]["origin"], "external");
    assert_eq!(accepted["run"]["run_id"], run_id.to_string());
    assert_eq!(accepted["run"]["status"], "queued");
    assert_eq!(accepted["run"]["state_version"], 1);
    assert_eq!(accepted["run"]["max_turns"], 12);
    assert_eq!(accepted["run"]["max_failures_per_turn"], 2);
    assert_eq!(
        accepted["run"]["resolved_config"],
        json!({
            "model": "gpt-5",
            "sampling": {"temperature": 0.2, "top_p": 0.9}
        })
    );

    activate_replacement_revision(&app, &harness_id).await;
    let retried = app
        .control(Method::POST, &format!("/v1/sessions/{session_id}/runs"))
        .json(&command)
        .send()
        .await
        .expect("retry run");
    assert_eq!(retried.status(), StatusCode::OK);
    assert_eq!(
        retried.json::<Value>().await.expect("retried run"),
        accepted
    );

    let fetched = app
        .control(Method::GET, &format!("/v1/runs/{run_id}"))
        .send()
        .await
        .expect("get run");
    assert_eq!(fetched.status(), StatusCode::OK);
    assert_eq!(
        fetched.json::<Value>().await.expect("fetched run"),
        accepted["run"]
    );

    let session = app
        .control(Method::GET, &format!("/v1/sessions/{session_id}"))
        .send()
        .await
        .expect("get session")
        .json::<Value>()
        .await
        .expect("session body");
    assert_eq!(session["current_revision"], 1);

    let mut conflicting = command.clone();
    conflicting["input"]["message"]["content"][0]["content"] = json!("different input");
    let conflicting = app
        .control(Method::POST, &format!("/v1/sessions/{session_id}/runs"))
        .json(&conflicting)
        .send()
        .await
        .expect("conflicting retry");
    assert_error(conflicting, StatusCode::CONFLICT, "run_id_conflict").await;

    let mut second_command = start_command(Uuid::now_v7(), Uuid::now_v7(), &harness_id);
    second_command["expected_session_revision"] = json!(1);
    let second = app
        .control(Method::POST, &format!("/v1/sessions/{session_id}/runs"))
        .json(&second_command)
        .send()
        .await
        .expect("second active run");
    assert_error(second, StatusCode::CONFLICT, "session_has_active_run").await;
}

#[tokio::test]
#[ignore = "requires AGENT_TEST_DATABASE_URL"]
async fn rejects_invalid_configuration_without_advancing_the_session() {
    let app = test_app().await;
    let harness_id = unique("invalid-config-harness");
    let revision_id = unique("invalid-config-revision");
    create_harness_revision(&app, &harness_id, &revision_id).await;
    let session_id = create_session(&app).await;
    let message_id = Uuid::now_v7();
    let mut command = start_command(Uuid::now_v7(), message_id, &harness_id);
    command["config_override"] = json!({"sampling": {"temperature": -1}});

    let response = app
        .control(Method::POST, &format!("/v1/sessions/{session_id}/runs"))
        .json(&command)
        .send()
        .await
        .expect("invalid configuration response");
    assert_error(
        response,
        StatusCode::UNPROCESSABLE_ENTITY,
        "invalid_harness_configuration",
    )
    .await;

    let session_revision: i64 =
        sqlx::query_scalar("select current_revision from sessions where session_id = $1")
            .bind(session_id)
            .fetch_one(&app.pool)
            .await
            .expect("session revision");
    assert_eq!(session_revision, 0);
    let message_exists: bool = sqlx::query_scalar(
        "select exists(select 1 from session_messages where session_message_id = $1)",
    )
    .bind(message_id)
    .fetch_one(&app.pool)
    .await
    .expect("message existence");
    assert!(!message_exists);
}

async fn create_harness_revision(app: &support::TestApp, harness_id: &str, revision_id: &str) {
    let harness = app
        .control(Method::POST, "/v1/harnesses")
        .json(&json!({
            "harness_id": harness_id,
            "slug": unique("execution"),
            "display_name": "Execution test harness"
        }))
        .send()
        .await
        .expect("create harness");
    assert_eq!(harness.status(), StatusCode::CREATED);

    let revision = app
        .control(
            Method::POST,
            &format!("/v1/harnesses/{harness_id}/revisions"),
        )
        .json(&revision_command(revision_id, "v1"))
        .send()
        .await
        .expect("create revision");
    assert_eq!(revision.status(), StatusCode::CREATED);
    activate_revision(app, harness_id, revision_id).await;

    let enabled = app
        .control(Method::PUT, &format!("/v1/harnesses/{harness_id}/enabled"))
        .json(&json!({"enabled": true}))
        .send()
        .await
        .expect("enable harness");
    assert_eq!(enabled.status(), StatusCode::OK);
}

async fn activate_replacement_revision(app: &support::TestApp, harness_id: &str) {
    let revision_id = unique("replacement-revision");
    let revision = app
        .control(
            Method::POST,
            &format!("/v1/harnesses/{harness_id}/revisions"),
        )
        .json(&revision_command(&revision_id, "v2"))
        .send()
        .await
        .expect("create replacement revision");
    assert_eq!(revision.status(), StatusCode::CREATED);
    activate_revision(app, harness_id, &revision_id).await;
}

async fn activate_revision(app: &support::TestApp, harness_id: &str, revision_id: &str) {
    let activated = app
        .control(
            Method::PUT,
            &format!("/v1/harnesses/{harness_id}/active-revision"),
        )
        .json(&json!({"harness_revision_id": revision_id}))
        .send()
        .await
        .expect("activate revision");
    assert_eq!(activated.status(), StatusCode::OK);
}

async fn create_session(app: &support::TestApp) -> Uuid {
    let session_id = Uuid::now_v7();
    let response = app
        .control(Method::POST, "/v1/sessions")
        .json(&json!({"session_id": session_id}))
        .send()
        .await
        .expect("create session");
    assert_eq!(response.status(), StatusCode::CREATED);
    session_id
}

fn revision_command(revision_id: &str, revision: &str) -> Value {
    json!({
        "harness_revision_id": revision_id,
        "revision": revision,
        "contract_version": 1,
        "default_config": {
            "model": "gpt-5",
            "sampling": {"temperature": 1.0, "top_p": 0.9},
            "removed": true
        },
        "config_schema": {
            "type": "object",
            "properties": {
                "model": {"type": "string"},
                "sampling": {
                    "type": "object",
                    "properties": {
                        "temperature": {"type": "number", "minimum": 0},
                        "top_p": {"type": "number", "minimum": 0, "maximum": 1}
                    },
                    "required": ["temperature", "top_p"]
                }
            },
            "required": ["model", "sampling"]
        }
    })
}

fn start_command(run_id: Uuid, message_id: Uuid, harness_id: &str) -> Value {
    json!({
        "run_id": run_id,
        "input": {
            "session_message_id": message_id,
            "message": user_message(message_id, "start the run")
        },
        "harness": {
            "selection": "active_revision",
            "harness_id": harness_id
        },
        "config_override": {},
        "limits": {},
        "expected_session_revision": 0
    })
}

fn user_message(message_id: Uuid, content: &str) -> Value {
    json!({
        "role": "user",
        "id": message_id.to_string(),
        "timestamp": 1,
        "content": [{"type": "text", "content": content}]
    })
}
