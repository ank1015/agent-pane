mod support;

use reqwest::{Method, StatusCode};
use serde_json::{Value, json};

use support::{assert_error, test_app, unique};

#[tokio::test]
#[ignore = "requires AGENT_TEST_DATABASE_URL pointing to a PostgreSQL test database"]
async fn revisions_follow_the_derived_lifecycle() {
    let app = test_app().await;
    let harness_id = unique("revision-harness");
    let harness_path = format!("/v1/harnesses/{harness_id}");
    let create_harness = app
        .control(Method::POST, "/v1/harnesses")
        .json(&json!({
            "harness_id": harness_id,
            "slug": unique("revision-api"),
            "display_name": "Revision API harness"
        }))
        .send()
        .await
        .expect("create harness");
    assert_eq!(create_harness.status(), StatusCode::CREATED);

    let invalid_schema = app
        .control(Method::POST, &format!("{harness_path}/revisions"))
        .json(&json!({
            "harness_revision_id": unique("invalid-schema"),
            "revision": "invalid-schema",
            "contract_version": 1,
            "default_config": {},
            "config_schema": { "type": 7 }
        }))
        .send()
        .await
        .expect("invalid schema response");
    assert_error(
        invalid_schema,
        StatusCode::UNPROCESSABLE_ENTITY,
        "invalid_config_schema",
    )
    .await;

    let invalid_default = app
        .control(Method::POST, &format!("{harness_path}/revisions"))
        .json(&json!({
            "harness_revision_id": unique("invalid-default"),
            "revision": "invalid-default",
            "contract_version": 1,
            "default_config": { "turns": 0 },
            "config_schema": {
                "type": "object",
                "properties": { "turns": { "type": "integer", "minimum": 1 } }
            }
        }))
        .send()
        .await
        .expect("invalid default response");
    assert_error(
        invalid_default,
        StatusCode::UNPROCESSABLE_ENTITY,
        "default_config_invalid",
    )
    .await;

    let revision_1 = unique("revision");
    let register_1 = json!({
        "harness_revision_id": revision_1,
        "revision": "2026-08-24",
        "contract_version": 1,
        "default_config": { "turns": 4 },
        "config_schema": {
            "$schema": "https://json-schema.org/draft/2020-12/schema",
            "type": "object",
            "properties": { "turns": { "type": "integer", "minimum": 1 } },
            "required": ["turns"]
        }
    });
    let first = app
        .control(Method::POST, &format!("{harness_path}/revisions"))
        .json(&register_1)
        .send()
        .await
        .expect("register first revision");
    assert_eq!(first.status(), StatusCode::CREATED);
    assert_eq!(
        first.json::<Value>().await.expect("first revision body")["status"],
        "registered"
    );

    let retry = app
        .control(Method::POST, &format!("{harness_path}/revisions"))
        .json(&register_1)
        .send()
        .await
        .expect("retry first revision");
    assert_eq!(retry.status(), StatusCode::OK);

    let active = app
        .control(Method::PUT, &format!("{harness_path}/active-revision"))
        .json(&json!({ "harness_revision_id": revision_1 }))
        .send()
        .await
        .expect("activate first revision");
    assert_eq!(active.status(), StatusCode::OK);
    let active = active.json::<Value>().await.expect("activation body");
    assert_eq!(active["active_revision"]["status"], "active");
    assert_eq!(active["harness"]["active_revision_id"], revision_1);

    let enabled = app
        .control(Method::PUT, &format!("{harness_path}/enabled"))
        .json(&json!({ "enabled": true }))
        .send()
        .await
        .expect("enable harness");
    assert_eq!(enabled.status(), StatusCode::OK);

    let clear_enabled = app
        .control(Method::DELETE, &format!("{harness_path}/active-revision"))
        .send()
        .await
        .expect("clear enabled harness response");
    assert_error(clear_enabled, StatusCode::CONFLICT, "harness_enabled").await;

    let retire_active = app
        .control(
            Method::POST,
            &format!("{harness_path}/revisions/{revision_1}/retire"),
        )
        .send()
        .await
        .expect("retire active response");
    assert_error(
        retire_active,
        StatusCode::CONFLICT,
        "active_revision_cannot_be_retired",
    )
    .await;

    let revision_2 = unique("revision");
    let second = app
        .control(Method::POST, &format!("{harness_path}/revisions"))
        .json(&json!({
            "harness_revision_id": revision_2,
            "revision": "2026-08-25",
            "contract_version": 1,
            "default_config": {},
            "config_schema": null
        }))
        .send()
        .await
        .expect("register second revision");
    assert_eq!(second.status(), StatusCode::CREATED);

    let activate_second = app
        .control(Method::PUT, &format!("{harness_path}/active-revision"))
        .json(&json!({ "harness_revision_id": revision_2 }))
        .send()
        .await
        .expect("activate second revision");
    assert_eq!(activate_second.status(), StatusCode::OK);

    let deprecated = app
        .control(
            Method::GET,
            &format!("{harness_path}/revisions?status=deprecated"),
        )
        .send()
        .await
        .expect("deprecated list");
    let deprecated = deprecated
        .json::<Value>()
        .await
        .expect("deprecated list body");
    assert_eq!(deprecated["items"].as_array().expect("items").len(), 1);
    assert_eq!(deprecated["items"][0]["harness_revision_id"], revision_1);

    let retired = app
        .control(
            Method::POST,
            &format!("{harness_path}/revisions/{revision_1}/retire"),
        )
        .send()
        .await
        .expect("retire old revision");
    assert_eq!(retired.status(), StatusCode::OK);
    assert_eq!(
        retired
            .json::<Value>()
            .await
            .expect("retired revision body")["status"],
        "retired"
    );

    let reactivate_retired = app
        .control(Method::PUT, &format!("{harness_path}/active-revision"))
        .json(&json!({ "harness_revision_id": revision_1 }))
        .send()
        .await
        .expect("reactivate retired response");
    assert_error(
        reactivate_retired,
        StatusCode::CONFLICT,
        "retired_revision_cannot_be_activated",
    )
    .await;

    let disabled = app
        .control(Method::PUT, &format!("{harness_path}/enabled"))
        .json(&json!({ "enabled": false }))
        .send()
        .await
        .expect("disable harness");
    assert_eq!(disabled.status(), StatusCode::OK);
    let cleared = app
        .control(Method::DELETE, &format!("{harness_path}/active-revision"))
        .send()
        .await
        .expect("clear active revision");
    assert_eq!(cleared.status(), StatusCode::OK);
    assert_eq!(
        cleared.json::<Value>().await.expect("cleared harness body")["active_revision_id"],
        Value::Null
    );
}
