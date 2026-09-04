use std::{
    env,
    sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    },
    time::Duration,
};

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
use tokio::{
    io::AsyncWriteExt,
    net::{TcpListener, TcpStream},
    sync::Semaphore,
    task::JoinHandle,
};

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

    let (provider_url, provider_task, provider_calls, provider_release) = spawn_provider().await;
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

    verify_runs(
        &client,
        &gateway_url,
        &database,
        &provider_calls,
        &provider_release,
    )
    .await;

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

async fn spawn_provider() -> (String, JoinHandle<()>, Arc<AtomicUsize>, Arc<Semaphore>) {
    let calls = Arc::new(AtomicUsize::new(0));
    let release = Arc::new(Semaphore::new(1));
    let handler_calls = calls.clone();
    let handler_release = release.clone();
    let app = Router::new().route(
        "/v1/responses",
        post(move || {
            let calls = handler_calls.clone();
            let release = handler_release.clone();
            async move {
                calls.fetch_add(1, Ordering::SeqCst);
                release.acquire().await.expect("release provider").forget();
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
            }
        }),
    );
    let (url, task) = spawn_app(app).await;
    (url, task, calls, release)
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
        "truncate table llm_runs, llm_usage, llm_requests, provider_credentials, provider_accounts cascade",
    )
    .execute(database.pool())
    .await
    .expect("reset disposable test database");
}

async fn verify_runs(
    client: &Client,
    url: &str,
    database: &Database,
    calls: &AtomicUsize,
    release: &Semaphore,
) {
    let endpoint = format!("{url}/v1/llm/runs");
    let unauthorized = client
        .post(&endpoint)
        .json(&llm_request())
        .send()
        .await
        .unwrap();
    assert_eq!(unauthorized.status(), StatusCode::UNAUTHORIZED);
    let missing_key = client
        .post(&endpoint)
        .bearer_auth(API_TOKEN)
        .json(&llm_request())
        .send()
        .await
        .unwrap();
    assert_eq!(missing_key.status(), StatusCode::BAD_REQUEST);

    // Send a complete submission but discard the connection without reading its
    // response. The caller has no run ID to save.
    let body = llm_request().to_string();
    let mut socket = TcpStream::connect(url.trim_start_matches("http://"))
        .await
        .unwrap();
    socket.write_all(format!("POST /v1/llm/runs HTTP/1.1\r\nHost: localhost\r\nAuthorization: Bearer {API_TOKEN}\r\nIdempotency-Key: lost-submission\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\r\n{body}", body.len()).as_bytes()).await.unwrap();
    tokio::time::timeout(Duration::from_secs(3), async {
        while calls.load(Ordering::SeqCst) < 2 {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("detached provider started");
    drop(socket);

    let submit = || {
        client
            .post(&endpoint)
            .bearer_auth(API_TOKEN)
            .header("Idempotency-Key", "lost-submission")
            .json(&llm_request())
            .send()
    };
    let (first, second) = tokio::join!(submit(), submit());
    let first = first.unwrap();
    assert_eq!(first.status(), StatusCode::ACCEPTED);
    assert!(first.headers().contains_key("location"));
    let first: Value = first.json().await.unwrap();
    let second: Value = second.unwrap().json().await.unwrap();
    assert_eq!(first["run_id"], second["run_id"]);
    assert_eq!(calls.load(Ordering::SeqCst), 2);
    let run_url = format!("{endpoint}/{}", first["run_id"].as_str().unwrap());

    let mut changed = llm_request();
    changed["request"]["instructions"] = json!("Different request");
    let conflict = client
        .post(&endpoint)
        .bearer_auth(API_TOKEN)
        .header("Idempotency-Key", "lost-submission")
        .json(&changed)
        .send()
        .await
        .unwrap();
    assert_eq!(conflict.status(), StatusCode::CONFLICT);
    assert_eq!(
        client.get(&run_url).send().await.unwrap().status(),
        StatusCode::UNAUTHORIZED
    );
    assert_eq!(
        client
            .get(format!("{run_url}?wait_seconds=26"))
            .bearer_auth(API_TOKEN)
            .send()
            .await
            .unwrap()
            .status(),
        StatusCode::BAD_REQUEST
    );
    // An abandoned await must not cancel the provider operation.
    assert!(
        client
            .get(format!("{run_url}?wait_seconds=25"))
            .bearer_auth(API_TOKEN)
            .timeout(Duration::from_millis(50))
            .send()
            .await
            .is_err()
    );
    release.add_permits(1);
    let result: Value = client
        .get(format!("{run_url}?wait_seconds=5"))
        .bearer_auth(API_TOKEN)
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(result["status"], "succeeded");
    assert_eq!(result["result"]["message"]["id"], "response-integration");
    let replay = submit().await.unwrap();
    assert_eq!(replay.status(), StatusCode::OK);
    assert_eq!(replay.json::<Value>().await.unwrap(), result);
    assert_eq!(calls.load(Ordering::SeqCst), 2);

    let id = first["run_id"]
        .as_str()
        .unwrap()
        .parse::<uuid::Uuid>()
        .unwrap();
    sqlx::query("update llm_runs set expires_at = now() - interval '1 second' where id = $1")
        .bind(id)
        .execute(database.pool())
        .await
        .unwrap();
    assert_eq!(
        client
            .get(&run_url)
            .bearer_auth(API_TOKEN)
            .send()
            .await
            .unwrap()
            .status(),
        StatusCode::GONE
    );
    assert_eq!(submit().await.unwrap().status(), StatusCode::GONE);
    database.cleanup_runs().await.unwrap();
    let stored: (String, Option<Value>) =
        sqlx::query_as("select status, result from llm_runs where id = $1")
            .bind(id)
            .fetch_one(database.pool())
            .await
            .unwrap();
    assert_eq!(stored, ("expired".to_owned(), None));
    assert_eq!(calls.load(Ordering::SeqCst), 2);

    let mut invalid = llm_request();
    invalid["request"]["model"]["id"] = json!("unknown-model");
    let failed: Value = client
        .post(&endpoint)
        .bearer_auth(API_TOKEN)
        .header("Idempotency-Key", "failed-run")
        .json(&invalid)
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let failed_url = format!(
        "{endpoint}/{}?wait_seconds=5",
        failed["run_id"].as_str().unwrap()
    );
    let failed: Value = client
        .get(failed_url)
        .bearer_auth(API_TOKEN)
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(failed["status"], "failed");
    assert_eq!(failed["result"]["error"]["kind"], "unknown_model");
    assert_eq!(calls.load(Ordering::SeqCst), 2);

    let new_submission = || {
        client
            .post(&endpoint)
            .bearer_auth(API_TOKEN)
            .header("Idempotency-Key", "concurrent-first-submission")
            .json(&llm_request())
            .send()
    };
    let (one, two) = tokio::join!(new_submission(), new_submission());
    let one: Value = one.unwrap().json().await.unwrap();
    let two: Value = two.unwrap().json().await.unwrap();
    assert_eq!(one["run_id"], two["run_id"]);
    release.add_permits(1);
    let completed: Value = client
        .get(format!(
            "{endpoint}/{}?wait_seconds=5",
            one["run_id"].as_str().unwrap()
        ))
        .bearer_auth(API_TOKEN)
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(completed["status"], "succeeded");
    assert_eq!(calls.load(Ordering::SeqCst), 3);
}
