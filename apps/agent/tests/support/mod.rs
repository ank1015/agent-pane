#![allow(dead_code)]

use agent::{
    AppState, Database,
    auth::{ControlToken, HarnessToken},
    execution::ExecutionPolicy,
    router,
};
use reqwest::{Client, Method, RequestBuilder, Response, StatusCode, header::CACHE_CONTROL};
use serde_json::Value;
use sqlx::{PgPool, postgres::PgPoolOptions};
use std::time::Duration;
use tokio::{net::TcpListener, task::JoinHandle};
use uuid::Uuid;

pub const CONTROL_TOKEN: &str = "agent-control-token-that-is-long-enough";
pub const HARNESS_TOKEN: &str = "agent-harness-token-that-is-long-enough";

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
    pub fn harness(&self, method: Method, path: &str) -> RequestBuilder {
        self.client
            .request(method, format!("{}{path}", self.base_url))
            .bearer_auth(HARNESS_TOKEN)
    }
}
impl Drop for TestApp {
    fn drop(&mut self) {
        self.server.abort();
    }
}

pub async fn test_app() -> TestApp {
    let url =
        std::env::var("AGENT_TEST_DATABASE_URL").expect("AGENT_TEST_DATABASE_URL must be set");
    let pool = PgPoolOptions::new()
        .max_connections(5)
        .connect(&url)
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
    let state = AppState::new(
        Database::from_pool(pool.clone()),
        ControlToken::new(CONTROL_TOKEN).unwrap(),
        HarnessToken::new(HARNESS_TOKEN).unwrap(),
        ExecutionPolicy::default(),
    );
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let server = tokio::spawn(async move {
        axum::serve(listener, router(state, 1024 * 1024))
            .await
            .unwrap();
    });
    TestApp {
        client: Client::new(),
        base_url: format!("http://{address}"),
        pool,
        server,
    }
}
pub async fn assert_error(response: Response, status: StatusCode, code: &str) -> Value {
    assert_eq!(response.status(), status);
    assert_eq!(response.headers().get(CACHE_CONTROL).unwrap(), "no-store");
    let body = response.json::<Value>().await.unwrap();
    assert_eq!(body["error"]["code"], code);
    body
}
pub fn unique(prefix: &str) -> String {
    format!("{prefix}-{}", Uuid::now_v7())
}
