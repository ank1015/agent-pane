mod support;

use reqwest::{Method, StatusCode};
use serde_json::{Value, json};

use support::{assert_error, lazy_test_app, test_app, unique};

#[tokio::test]
async fn harness_routes_require_control_authentication() {
    let app = lazy_test_app().await;

    let unauthorized = app
        .client
        .get(format!("{}/v1/harnesses", app.base_url))
        .send()
        .await
        .expect("unauthorized response");
    assert_error(unauthorized, StatusCode::UNAUTHORIZED, "unauthorized").await;

    let invalid = app
        .control(Method::POST, "/v1/harnesses")
        .json(&json!({
            "harness_id": " harness-1",
            "slug": "Not-A-Slug",
            "display_name": ""
        }))
        .send()
        .await
        .expect("validation response");
    let body = assert_error(invalid, StatusCode::BAD_REQUEST, "invalid_request").await;
    assert!(body["error"]["details"]["issues"].is_array());
}

#[tokio::test]
#[ignore = "requires AGENT_TEST_DATABASE_URL pointing to a PostgreSQL test database"]
async fn harness_crud_supports_safe_retries_and_filters() {
    let app = test_app().await;
    let harness_id = unique("harness");
    let slug = unique("api-harness");
    let create = json!({
        "harness_id": harness_id,
        "slug": slug,
        "display_name": "API harness",
        "description": "Original description",
        "supported_providers": [
            {"provider_id": "openai", "model_ids": ["gpt-5.6-sol", "gpt-5.6-terra"]}
        ],
        "supported_reasoning_levels": ["low", "medium", "high", "xhigh", "max"]
    });

    let first = app
        .control(Method::POST, "/v1/harnesses")
        .json(&create)
        .send()
        .await
        .expect("create harness");
    assert_eq!(first.status(), StatusCode::CREATED);
    let first_body = first.json::<Value>().await.expect("created harness body");
    assert_eq!(first_body["enabled"], false);
    assert_eq!(first_body["active_revision_id"], Value::Null);
    assert_eq!(
        first_body["supported_providers"],
        create["supported_providers"]
    );
    assert_eq!(
        first_body["supported_reasoning_levels"],
        create["supported_reasoning_levels"]
    );
    assert!(first_body["created_at"].is_string());
    assert!(first_body["updated_at"].is_string());

    let retry = app
        .control(Method::POST, "/v1/harnesses")
        .json(&create)
        .send()
        .await
        .expect("retry create harness");
    assert_eq!(retry.status(), StatusCode::OK);
    assert_eq!(
        retry.json::<Value>().await.expect("retried harness body"),
        first_body
    );

    let id_conflict = app
        .control(Method::POST, "/v1/harnesses")
        .json(&json!({
            "harness_id": harness_id,
            "slug": unique("different-slug"),
            "display_name": "Different"
        }))
        .send()
        .await
        .expect("id conflict response");
    assert_error(id_conflict, StatusCode::CONFLICT, "harness_id_conflict").await;

    let slug_conflict = app
        .control(Method::POST, "/v1/harnesses")
        .json(&json!({
            "harness_id": unique("different-harness"),
            "slug": slug,
            "display_name": "Different"
        }))
        .send()
        .await
        .expect("slug conflict response");
    assert_error(slug_conflict, StatusCode::CONFLICT, "harness_slug_conflict").await;

    let updated = app
        .control(Method::PATCH, &format!("/v1/harnesses/{harness_id}"))
        .json(&json!({
            "display_name": "Renamed harness",
            "description": null,
            "supported_providers": [
                {"provider_id": "anthropic", "model_ids": ["claude-opus-4-1"]}
            ],
            "supported_reasoning_levels": ["low", "high"]
        }))
        .send()
        .await
        .expect("update harness");
    assert_eq!(updated.status(), StatusCode::OK);
    let updated = updated.json::<Value>().await.expect("updated harness body");
    assert_eq!(updated["display_name"], "Renamed harness");
    assert_eq!(updated["description"], Value::Null);
    assert_eq!(
        updated["supported_providers"][0]["provider_id"],
        "anthropic"
    );
    assert_eq!(
        updated["supported_reasoning_levels"],
        json!(["low", "high"])
    );
    assert!(updated["updated_at"].is_string());

    let filtered = app
        .control(
            Method::GET,
            &format!("/v1/harnesses?slug={slug}&enabled=false"),
        )
        .send()
        .await
        .expect("filtered harness list");
    assert_eq!(filtered.status(), StatusCode::OK);
    let filtered = filtered.json::<Value>().await.expect("harness list body");
    assert_eq!(filtered["items"].as_array().expect("items").len(), 1);
    assert_eq!(filtered["items"][0]["harness_id"], harness_id);

    let second_harness = app
        .control(Method::POST, "/v1/harnesses")
        .json(&json!({
            "harness_id": unique("page-harness"),
            "slug": unique("page-harness"),
            "display_name": "Pagination harness"
        }))
        .send()
        .await
        .expect("create pagination harness");
    assert_eq!(second_harness.status(), StatusCode::CREATED);

    let page_1 = app
        .control(Method::GET, "/v1/harnesses?limit=1")
        .send()
        .await
        .expect("first harness page")
        .json::<Value>()
        .await
        .expect("first harness page body");
    let cursor = page_1["next_cursor"]
        .as_str()
        .expect("a second harness exists");
    let page_2 = app
        .control(
            Method::GET,
            &format!("/v1/harnesses?limit=1&cursor={cursor}"),
        )
        .send()
        .await
        .expect("second harness page")
        .json::<Value>()
        .await
        .expect("second harness page body");
    assert_ne!(
        page_1["items"][0]["harness_id"],
        page_2["items"][0]["harness_id"]
    );

    let enable = app
        .control(Method::PUT, &format!("/v1/harnesses/{harness_id}/enabled"))
        .json(&json!({ "enabled": true }))
        .send()
        .await
        .expect("enable response");
    assert_error(enable, StatusCode::CONFLICT, "no_active_revision").await;
}
