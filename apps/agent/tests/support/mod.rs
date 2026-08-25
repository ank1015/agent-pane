#![allow(dead_code)]

use std::time::Duration;

use agent::{
    AppState, Database,
    auth::{ControlToken, WorkerToken},
    execution::ExecutionPolicy,
    router,
};
use base64::{Engine as _, engine::general_purpose::URL_SAFE_NO_PAD};
use reqwest::{Client, Method, RequestBuilder, Response, StatusCode, header::CACHE_CONTROL};
use serde_json::{Value, json};
use sqlx::{PgPool, postgres::PgPoolOptions};
use tokio::{net::TcpListener, task::JoinHandle};
use uuid::Uuid;

pub const CONTROL_TOKEN: &str = "agent-control-token-that-is-long-enough";
pub const WORKER_TOKEN: &str = "agent-worker-token-that-is-long-enough";

pub struct TestApp {
    pub client: Client,
    pub base_url: String,
    pub pool: PgPool,
    server: JoinHandle<()>,
}

impl TestApp {
    pub fn control(&self, method: Method, path: &str) -> RequestBuilder {
        self.client
            .request(method, format!("{}{path}", self.base_url))
            .bearer_auth(CONTROL_TOKEN)
    }

    pub fn worker(&self, method: Method, path: &str) -> RequestBuilder {
        self.client
            .request(method, format!("{}{path}", self.base_url))
            .bearer_auth(WORKER_TOKEN)
    }
}

impl Drop for TestApp {
    fn drop(&mut self) {
        self.server.abort();
    }
}

pub async fn test_app() -> TestApp {
    let database_url =
        std::env::var("AGENT_TEST_DATABASE_URL").expect("AGENT_TEST_DATABASE_URL must be set");
    let pool = PgPoolOptions::new()
        .max_connections(5)
        .connect(&database_url)
        .await
        .expect("test database connection");
    agent::migrate(&pool).await.expect("agent migrations");
    spawn(pool).await
}

pub async fn lazy_test_app() -> TestApp {
    let pool = PgPoolOptions::new()
        .acquire_timeout(Duration::from_millis(50))
        .connect_lazy("postgres://postgres:postgres@127.0.0.1:1/agent")
        .expect("lazy test pool");
    spawn(pool).await
}

async fn spawn(pool: PgPool) -> TestApp {
    let test_pool = pool.clone();
    let state = AppState::new(
        Database::from_pool(pool),
        ControlToken::new(CONTROL_TOKEN).expect("control token"),
        WorkerToken::new(WORKER_TOKEN).expect("worker token"),
        ExecutionPolicy::default(),
    );
    let listener = TcpListener::bind("127.0.0.1:0")
        .await
        .expect("test listener");
    let address = listener.local_addr().expect("listener address");
    let server = tokio::spawn(async move {
        axum::serve(listener, router(state, 1024 * 1024))
            .await
            .expect("test server");
    });
    TestApp {
        client: Client::new(),
        base_url: format!("http://{address}"),
        pool: test_pool,
        server,
    }
}

pub async fn assert_error(response: Response, status: StatusCode, code: &str) -> Value {
    assert_eq!(response.status(), status);
    assert_eq!(
        response
            .headers()
            .get(CACHE_CONTROL)
            .expect("cache control"),
        "no-store"
    );
    let body = response.json::<Value>().await.expect("error body");
    assert_eq!(body["error"]["code"], code);
    assert!(body["error"]["message"].is_string());
    body
}

pub fn unique(prefix: &str) -> String {
    format!("{prefix}-{}", Uuid::now_v7())
}

pub struct ExecutionFixture {
    pub run_id: Uuid,
    pub revision_id: String,
}

pub async fn start_execution_fixture(
    app: &TestApp,
    max_turns: u32,
    max_failures: u32,
) -> ExecutionFixture {
    let harness_id = unique("lifecycle-harness");
    let revision_id = unique("lifecycle-revision");
    let harness = app
        .control(Method::POST, "/v1/harnesses")
        .json(&json!({
            "harness_id": harness_id,
            "slug": unique("lifecycle"),
            "display_name": "Execution lifecycle test"
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

    let session_id = Uuid::now_v7();
    let session = app
        .control(Method::POST, "/v1/sessions")
        .json(&json!({"session_id": session_id}))
        .send()
        .await
        .expect("create session");
    assert_eq!(session.status(), StatusCode::CREATED);
    let run_id = Uuid::now_v7();
    let message_id = Uuid::now_v7();
    let run = app
        .control(Method::POST, &format!("/v1/sessions/{session_id}/runs"))
        .json(&json!({
            "run_id": run_id,
            "input": {
                "session_message_id": message_id,
                "message": user_message(message_id, "start")
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
    assert_eq!(run.status(), StatusCode::ACCEPTED);
    ExecutionFixture {
        run_id,
        revision_id,
    }
}

pub async fn claim_execution(
    app: &TestApp,
    fixture: &ExecutionFixture,
    token_byte: u8,
) -> (String, Value) {
    let token = lease_token(token_byte);
    let response = app
        .worker(Method::POST, "/v1/worker/runs/claim")
        .header("x-agent-lease-token", &token)
        .json(&json!({
            "lease_id": Uuid::now_v7(),
            "worker_instance_id": "lifecycle-worker",
            "supported_harness_revision_ids": [fixture.revision_id]
        }))
        .send()
        .await
        .expect("claim run");
    assert_eq!(response.status(), StatusCode::OK);
    let body = response.json().await.expect("claim body");
    (token, body)
}

pub fn lease_token(byte: u8) -> String {
    URL_SAFE_NO_PAD.encode([byte; 32])
}

pub fn user_message(message_id: Uuid, content: &str) -> Value {
    json!({
        "role": "user",
        "id": message_id.to_string(),
        "timestamp": 1,
        "content": [{"type": "text", "content": content}]
    })
}
