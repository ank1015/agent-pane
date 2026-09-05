use axum::{Json, Router, body::to_bytes, extract::Request, response::IntoResponse};
use execution_client::{
    ExecutionClient, ExecutionClientConfig, HostFilter, SnapshotFilter, api::*,
};
use execution_core::{ExecutionErrorCode, OperationContext};
use serde_json::{Value, json};
use std::{
    sync::{
        Arc, Mutex,
        atomic::{AtomicUsize, Ordering},
    },
    time::Duration,
};
use uuid::Uuid;

struct Server {
    client: ExecutionClient,
    mode: Arc<AtomicUsize>,
    requests: Arc<Mutex<Vec<(String, Value)>>>,
    task: tokio::task::JoinHandle<()>,
}
impl Drop for Server {
    fn drop(&mut self) {
        self.task.abort();
    }
}
impl Server {
    async fn new() -> Self {
        let mode = Arc::new(AtomicUsize::new(0));
        let requests = Arc::new(Mutex::new(Vec::new()));
        let m = mode.clone();
        let seen = requests.clone();
        let app = Router::new().fallback(move |request: Request| {
            let mode = m.clone(); let seen = seen.clone();
            async move {
                assert_eq!(request.headers()["authorization"], "Bearer test-token");
                assert!(request.uri().path().starts_with("/gateway/v1/"));
                let path = request.uri().to_string();
                let method = request.method().clone();
                if method == axum::http::Method::POST && !path.ends_with("/resume") {
                    assert_eq!(request.headers()["idempotency-key"], "persisted-key");
                }
                let bytes = to_bytes(request.into_body(), 65536).await.unwrap();
                let body = if bytes.is_empty() { Value::Null } else { serde_json::from_slice(&bytes).unwrap() };
                seen.lock().unwrap().push((path.clone(), body));
                match mode.load(Ordering::SeqCst) {
                    1 => return "x".repeat(4096).into_response(),
                    2 => return "not-json".into_response(),
                    3 => tokio::time::sleep(Duration::from_secs(5)).await,
                    4 => return (axum::http::StatusCode::CONFLICT, Json(json!({"error":{"code":"IDEMPOTENCY_KEY_REUSED","message":"different request","retryable":false}}))).into_response(),
                    _ => {}
                }
                if method == axum::http::Method::GET && (path.contains('?') || path.ends_with("e2b-accounts")) { return Json(json!([])).into_response(); }
                let id = Uuid::nil(); let now = "2026-09-05T00:00:00Z";
                if path.contains("snapshots") {
                    Json(json!({"id":id,"e2b_account_id":id,"source_host_id":id,"name":"snapshot","desired_state":"ready","state":"creating","metadata":{},"created_at":now,"updated_at":now})).into_response()
                } else {
                    Json(json!({"id":id,"kind":"e2b","desired_state":"ready","state":"provisioning","status_retryable":false,"roots":[],"metadata":{},"revision":1,"created_at":now,"updated_at":now})).into_response()
                }
            }
        });
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let mut config = ExecutionClientConfig::new(
            format!("http://{}/gateway/", listener.local_addr().unwrap())
                .parse()
                .unwrap(),
            "test-token",
        );
        config.allow_insecure_http = true;
        config.max_response_bytes = 2048;
        let task = tokio::spawn(async move {
            axum::serve(listener, app).await.unwrap();
        });
        Self {
            client: ExecutionClient::new(config).unwrap(),
            mode,
            requests,
            task,
        }
    }
}

#[tokio::test]
async fn typed_management_preserves_prefix_filters_keys_and_resource_states() {
    let s = Server::new().await;
    let ctx = OperationContext::with_timeout(Duration::from_secs(5));
    let id = Uuid::nil();
    assert!(s.client.list_accounts(&ctx).await.unwrap().is_empty());
    assert!(
        s.client
            .list_hosts(
                &ctx,
                &HostFilter {
                    kind: Some(ExecutionHostKind::Registered),
                    ..Default::default()
                }
            )
            .await
            .unwrap()
            .is_empty()
    );
    s.client.get_host(&ctx, id).await.unwrap();
    let request = CreateExecutionHostRequest {
        name: None,
        source: E2bHostSource::Base {
            e2b_account_id: Some(id),
        },
        timeout_seconds: Some(3600),
        metadata: json!({}),
    };
    assert_eq!(
        s.client
            .create_host(&ctx, "persisted-key", &request)
            .await
            .unwrap()
            .state,
        ExecutionHostState::Provisioning
    );
    s.client.resume_host(&ctx, id).await.unwrap();
    s.client
        .list_snapshots(
            &ctx,
            &SnapshotFilter {
                state: Some(SnapshotState::Ready),
                e2b_account_id: Some(id),
                ..Default::default()
            },
        )
        .await
        .unwrap();
    s.client.get_snapshot(&ctx, id).await.unwrap();
    let request = CreateSnapshotRequest {
        name: Some("snapshot".into()),
        metadata: json!({}),
    };
    assert_eq!(
        s.client
            .create_snapshot(&ctx, id, "persisted-key", &request)
            .await
            .unwrap()
            .state,
        SnapshotState::Creating
    );
    let requests = s.requests.lock().unwrap();
    assert_eq!(requests.len(), 8);
    assert!(requests[1].0.contains("kind=registered"));
    assert!(requests[5].0.contains("state=ready"));
    assert_eq!(requests[3].1["source"]["type"], "base");
    assert_eq!(requests[7].1["name"], "snapshot");
}

#[tokio::test]
async fn management_bounds_responses_obeys_deadlines_and_does_not_retry_mutations() {
    let s = Server::new().await;
    let ctx = OperationContext::with_timeout(Duration::from_secs(5));
    s.mode.store(1, Ordering::SeqCst);
    assert_eq!(
        s.client.get_host(&ctx, Uuid::nil()).await.unwrap_err().code,
        ExecutionErrorCode::ResourceExhausted
    );
    s.mode.store(2, Ordering::SeqCst);
    assert!(s.client.list_accounts(&ctx).await.is_err());
    s.mode.store(3, Ordering::SeqCst);
    assert_eq!(
        s.client
            .list_accounts(&OperationContext::with_timeout(Duration::from_millis(20)))
            .await
            .unwrap_err()
            .code,
        ExecutionErrorCode::DeadlineExceeded
    );
    s.mode.store(4, Ordering::SeqCst);
    let request = CreateSnapshotRequest {
        name: None,
        metadata: json!({}),
    };
    let before = s.requests.lock().unwrap().len();
    assert!(
        s.client
            .create_snapshot(&ctx, Uuid::nil(), "persisted-key", &request)
            .await
            .is_err()
    );
    assert_eq!(s.requests.lock().unwrap().len(), before + 1);
    assert!(
        s.client
            .create_snapshot(&ctx, Uuid::nil(), "bad\nkey", &request)
            .await
            .is_err()
    );
    assert_eq!(s.requests.lock().unwrap().len(), before + 1);
}
