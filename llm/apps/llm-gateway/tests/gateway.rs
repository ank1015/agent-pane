use std::{env, time::Duration};

use axum::{Json, Router, routing::post};
use base64::{Engine as _, engine::general_purpose::STANDARD};
use llm_gateway::{
    account::AccountService,
    auth::AccessToken,
    config::DatabaseConfig,
    db::Database,
    gateway::{ConcurrencyConfig, Gateway},
    http,
    vault::Vault,
};
use reqwest::{Client, StatusCode};
use serde_json::{Value, json};
use tokio::{net::TcpListener, task::JoinHandle};

const API_TOKEN: &str = "runtime-test-token-with-at-least-32-bytes";
const ADMIN_TOKEN: &str = "admin-test-token-with-at-least-32-bytes!!";

#[tokio::test]
#[ignore = "requires LLM_GATEWAY_TEST_DATABASE_URL pointing to a disposable PostgreSQL database"]
async fn llm_call_is_authenticated_executed_and_accounted() {
    let database_url = env::var("LLM_GATEWAY_TEST_DATABASE_URL")
        .expect("LLM_GATEWAY_TEST_DATABASE_URL must point to a disposable PostgreSQL database");
    let database = Database::connect(&DatabaseConfig::new(database_url))
        .await
        .expect("connect to test database");
    database.migrate().await.expect("apply gateway migrations");
    reset_database(&database).await;

    let (provider_url, provider_task) = spawn_provider().await;
    let vault_key = STANDARD.encode([7_u8; 32]);
    let vault = Vault::from_base64(&vault_key).expect("valid test vault key");
    let accounts = AccountService::new(database.clone(), vault);
    let gateway = Gateway::new(
        database.clone(),
        accounts.clone(),
        ConcurrencyConfig {
            max_active_requests: 4,
            max_waiting_requests: 8,
            wait_timeout: Duration::from_secs(1),
        },
        Duration::from_secs(5),
        "unused-chatgpt-client-id".to_owned(),
        "http://127.0.0.1/unused-chatgpt-token-endpoint".to_owned(),
    )
    .expect("construct gateway HTTP clients");
    let app = http::router(
        database.clone(),
        gateway,
        accounts,
        AccessToken::new(API_TOKEN.as_bytes()).expect("valid runtime token"),
        AccessToken::new(ADMIN_TOKEN.as_bytes()).expect("valid admin token"),
        1024 * 1024,
        60,
    );
    let (gateway_url, gateway_task) = spawn_app(app).await;
    let client = Client::new();

    let health_without_token = client
        .get(format!("{gateway_url}/health"))
        .send()
        .await
        .expect("gateway responds to unauthenticated health check");
    assert_eq!(health_without_token.status(), StatusCode::UNAUTHORIZED);

    let health = client
        .get(format!("{gateway_url}/health"))
        .bearer_auth(API_TOKEN)
        .send()
        .await
        .expect("authenticated health check succeeds");
    assert_eq!(health.status(), StatusCode::OK);
    assert_eq!(
        health
            .headers()
            .get("x-content-type-options")
            .expect("security header"),
        "nosniff"
    );

    let unknown_without_token = client
        .get(format!("{gateway_url}/not-a-route"))
        .send()
        .await
        .expect("gateway responds to an unauthenticated unknown route");
    assert_eq!(unknown_without_token.status(), StatusCode::UNAUTHORIZED);

    let unknown_with_token = client
        .get(format!("{gateway_url}/not-a-route"))
        .bearer_auth(API_TOKEN)
        .send()
        .await
        .expect("gateway responds to an authenticated unknown route");
    assert_eq!(unknown_with_token.status(), StatusCode::NOT_FOUND);

    let admin_with_runtime_token = client
        .get(format!("{gateway_url}/v1/admin/accounts"))
        .bearer_auth(API_TOKEN)
        .send()
        .await
        .expect("gateway rejects runtime token on admin route");
    assert_eq!(admin_with_runtime_token.status(), StatusCode::UNAUTHORIZED);

    let unauthorized = client
        .post(format!("{gateway_url}/v1/llm"))
        .json(&llm_request())
        .send()
        .await
        .expect("gateway responds without a token");
    assert_eq!(unauthorized.status(), StatusCode::UNAUTHORIZED);

    let created = client
        .post(format!("{gateway_url}/v1/admin/accounts"))
        .bearer_auth(ADMIN_TOKEN)
        .json(&json!({
            "provider": "openai",
            "name": "integration-openai",
            "credentials": { "api_key": "provider-test-key" },
            "config": { "base_url": format!("{provider_url}/v1") },
            "make_default": true
        }))
        .send()
        .await
        .expect("create account request succeeds");
    assert_eq!(created.status(), StatusCode::CREATED);
    let created: Value = created.json().await.expect("account response is JSON");
    let account_id = created["account"]["id"]
        .as_str()
        .expect("account ID is present");

    let completed = client
        .post(format!("{gateway_url}/v1/llm"))
        .bearer_auth(API_TOKEN)
        .json(&llm_request())
        .send()
        .await
        .expect("LLM request succeeds");
    assert_eq!(completed.status(), StatusCode::OK);
    assert_eq!(
        completed
            .headers()
            .get("x-account-id")
            .expect("account header")
            .to_str()
            .expect("valid account header"),
        account_id
    );
    let completed: Value = completed.json().await.expect("completion response is JSON");
    assert_eq!(completed["account_id"], account_id);
    assert_eq!(completed["message"]["id"], "response-integration");
    assert_eq!(
        completed["message"]["content"][0]["response"]["content"],
        "Hello from the provider"
    );
    assert_eq!(completed["message"]["usage"]["input"], 7);
    assert_eq!(completed["message"]["usage"]["output"], 2);
    assert_eq!(completed["message"]["usage"]["cache_read"], 3);

    let requests = client
        .get(format!(
            "{gateway_url}/v1/admin/requests?provider=openai&operation=complete"
        ))
        .bearer_auth(ADMIN_TOKEN)
        .send()
        .await
        .expect("request history succeeds");
    assert_eq!(requests.status(), StatusCode::OK);
    let requests: Value = requests.json().await.expect("request history is JSON");
    assert_eq!(
        requests["items"].as_array().expect("request items").len(),
        1
    );
    assert_eq!(requests["items"][0]["status"], "succeeded");
    assert_eq!(requests["items"][0]["usage"]["input"], 7);

    let usage = client
        .get(format!(
            "{gateway_url}/v1/admin/usage?provider=openai&group_by=model"
        ))
        .bearer_auth(ADMIN_TOKEN)
        .send()
        .await
        .expect("usage report succeeds");
    assert_eq!(usage.status(), StatusCode::OK);
    let usage: Value = usage.json().await.expect("usage report is JSON");
    assert_eq!(usage["totals"]["request_count"], 1);
    assert_eq!(usage["totals"]["succeeded_count"], 1);
    assert_eq!(usage["totals"]["tokens"]["input"], 7);
    assert_eq!(usage["groups"][0]["key"], "gpt-5.6-luna");

    provider_task.abort();
    gateway_task.abort();
    reset_database(&database).await;
}

fn llm_request() -> Value {
    json!({
        "request": {
            "model": {
                "provider": "openai",
                "id": "gpt-5.6-luna"
            },
            "messages": [],
            "metadata": {
                "run_id": "integration-run"
            }
        }
    })
}

async fn spawn_provider() -> (String, JoinHandle<()>) {
    let app = Router::new().route(
        "/v1/responses",
        post(|| async {
            Json(json!({
                "id": "response-integration",
                "object": "response",
                "model": "gpt-5.6-luna",
                "status": "completed",
                "output": [{
                    "type": "message",
                    "content": [{
                        "type": "output_text",
                        "text": "Hello from the provider"
                    }]
                }],
                "usage": {
                    "input_tokens": 10,
                    "output_tokens": 2,
                    "input_tokens_details": { "cached_tokens": 3 }
                }
            }))
        }),
    );
    spawn_app(app).await
}

async fn spawn_app(app: Router) -> (String, JoinHandle<()>) {
    let listener = TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind test server");
    let address = listener.local_addr().expect("test server address");
    let task = tokio::spawn(async move {
        axum::serve(listener, app).await.expect("serve test app");
    });
    (format!("http://{address}"), task)
}

async fn reset_database(database: &Database) {
    sqlx::query(
        "truncate table llm_usage, llm_requests, provider_credentials, provider_accounts cascade",
    )
    .execute(database.pool())
    .await
    .expect("reset disposable test database");
}
