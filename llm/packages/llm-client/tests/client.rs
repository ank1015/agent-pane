use std::{
    convert::Infallible,
    sync::{
        Arc, Mutex,
        atomic::{AtomicUsize, Ordering},
    },
    time::Duration,
};

use axum::{
    Json, Router,
    body::Body,
    extract::{Path, Query},
    http::{HeaderMap, StatusCode},
    response::{IntoResponse, Response},
    routing::{get, post},
};
use llm_client::{
    ClientError, CompletionRequest, GatewayErrorKind, IdempotencyKey, LlmClient, LlmClientConfig,
    RunState, WaitOptions,
};
use serde_json::{Value, json};
use tokio::{net::TcpListener, task::JoinHandle};
use uuid::Uuid;

const TOKEN: &str = "private-runtime-test-token";

struct Server {
    url: url::Url,
    task: JoinHandle<()>,
}
impl Server {
    async fn new(router: Router) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("http://{}/prefix", listener.local_addr().unwrap())
            .parse()
            .unwrap();
        let task = tokio::spawn(async move {
            axum::serve(listener, Router::new().nest("/prefix", router))
                .await
                .unwrap();
        });
        Self { url, task }
    }
    fn config(&self) -> LlmClientConfig {
        let mut config = LlmClientConfig::new(self.url.clone(), TOKEN);
        config.allow_insecure_http = true;
        config
    }
    fn client(&self) -> LlmClient {
        LlmClient::new(self.config()).unwrap()
    }
}
impl Drop for Server {
    fn drop(&mut self) {
        self.task.abort();
    }
}

fn input() -> CompletionRequest {
    serde_json::from_value(json!({"account_id": Uuid::nil(), "request": {
        "model": {"provider": "openai", "id": "gpt-5.6-luna"},
        "messages": [], "instructions": "Answer concisely", "metadata": {"run_id": "logical-run"}
    }}))
    .unwrap()
}
fn key() -> IdempotencyKey {
    IdempotencyKey::new("logical-run/turn-1").unwrap()
}
fn snapshot(id: Uuid, status: &str) -> Value {
    let result = match status {
        "succeeded" => json!({"request_id": id, "account_id": Uuid::nil(), "message": {
            "id": "response-1", "model": {"provider": "openai", "id": "gpt-5.6-luna"},
            "duration_ms": 123, "native_message": {"opaque": [1,2,3]},
            "content": [{"type": "response", "response": {"content": "Hello"}}],
            "stop_reason": "stop", "timestamp": 1234
        }}),
        "failed" => json!({"request_id": id, "account_id": Uuid::nil(), "error": {
            "kind": "provider", "message": "provider is busy", "can_retry": true, "retry_after_ms": 1200,
            "provider_error": {"message": "rate limited", "can_retry": true, "http_status": 429,
                "provider_code": "quota", "provider_type": "rate_limit", "retry_after_ms": 1200}
        }}),
        _ => Value::Null,
    };
    json!({"run_id": id, "status": status, "result": result,
        "created_at": "2026-09-05T00:00:00Z",
        "completed_at": if status == "running" { Value::Null } else { json!("2026-09-05T00:01:00Z") },
        "expires_at": if status == "running" { Value::Null } else { json!("2026-09-07T00:01:00Z") }
    })
}

#[tokio::test]
async fn typed_submission_and_resumed_wait_use_gateway_contract() {
    let run_id = Uuid::now_v7();
    let seen = Arc::new(Mutex::new(Vec::new()));
    let submissions = seen.clone();
    let reads = Arc::new(AtomicUsize::new(0));
    let read_count = reads.clone();
    let server = Server::new(
        Router::new()
            .route(
                "/v1/llm/runs",
                post(move |headers: HeaderMap, Json(body): Json<Value>| {
                    let submissions = submissions.clone();
                    async move {
                        assert_eq!(headers["authorization"], format!("Bearer {TOKEN}"));
                        assert_eq!(headers["idempotency-key"], key().as_str());
                        submissions.lock().unwrap().push(body);
                        (StatusCode::ACCEPTED, Json(snapshot(run_id, "running")))
                    }
                }),
            )
            .route(
                "/v1/llm/runs/{id}",
                get(
                    move |Path(id): Path<Uuid>,
                          Query(query): Query<std::collections::HashMap<String, String>>,
                          headers: HeaderMap| {
                        let count = read_count.clone();
                        async move {
                            assert_eq!(id, run_id);
                            assert_eq!(headers["authorization"], format!("Bearer {TOKEN}"));
                            let index = count.fetch_add(1, Ordering::SeqCst);
                            assert_eq!(query["wait_seconds"], if index == 0 { "0" } else { "25" });
                            Json(snapshot(
                                id,
                                if index < 2 { "running" } else { "succeeded" },
                            ))
                        }
                    },
                ),
            ),
    )
    .await;
    let client = server.client();
    let first = client.submit(&key(), &input()).await.unwrap();
    let second = client.submit(&key(), &input()).await.unwrap();
    assert_eq!(first.run_id, second.run_id);
    assert!(matches!(first.state, RunState::Running));
    assert_eq!(
        *seen.lock().unwrap(),
        vec![serde_json::to_value(input()).unwrap(); 2]
    );
    assert!(matches!(
        client.get_run(run_id).await.unwrap().state,
        RunState::Running
    ));
    // Another client can await the saved ID without submitting again.
    let completion = server
        .client()
        .wait(run_id, WaitOptions::new(Duration::from_secs(3)))
        .await
        .unwrap();
    assert_eq!(completion.request_id, run_id);
    assert_eq!(completion.account_id, Uuid::nil());
    assert_eq!(
        completion.message.native_message,
        json!({"opaque": [1,2,3]})
    );
    assert_eq!(reads.load(Ordering::SeqCst), 3);
}

#[tokio::test]
async fn complete_uses_immediate_success_failure_and_expiry_without_retrieving() {
    for status in ["succeeded", "failed", "aborted", "expired"] {
        let id = Uuid::now_v7();
        let server = Server::new(Router::new().route(
            "/v1/llm/runs",
            post(move || async move {
                (
                    if status == "expired" {
                        StatusCode::GONE
                    } else {
                        StatusCode::OK
                    },
                    Json(snapshot(id, status)),
                )
            }),
        ))
        .await;
        let client = server.client();
        let run = client.submit(&key(), &input()).await.unwrap();
        assert_eq!(run.run_id, id);
        match client
            .complete(&key(), &input(), WaitOptions::default())
            .await
        {
            Ok(result) => {
                assert_eq!(status, "succeeded");
                assert_eq!(result.request_id, id);
            }
            Err(ClientError::RunFailed { run_id, failure }) => {
                assert_eq!(status, "failed");
                assert_eq!(run_id, id);
                assert_eq!(failure.error.kind, GatewayErrorKind::Provider);
                assert!(failure.error.can_retry);
                assert_eq!(
                    failure
                        .error
                        .provider_error
                        .unwrap()
                        .provider_code
                        .as_deref(),
                    Some("quota")
                );
            }
            Err(ClientError::RunAborted { run_id }) => {
                assert_eq!(status, "aborted");
                assert_eq!(run_id, id);
            }
            Err(ClientError::RunExpired { run_id }) => {
                assert_eq!(status, "expired");
                assert_eq!(run_id, id);
            }
            other => panic!("unexpected result: {other:?}"),
        }
    }
}

#[tokio::test]
async fn awaiting_reports_terminal_failure_and_expiration() {
    for status in ["failed", "aborted", "expired"] {
        let id = Uuid::now_v7();
        let server = Server::new(Router::new().route(
            "/v1/llm/runs/{id}",
            get(move || async move {
                (
                    if status == "expired" {
                        StatusCode::GONE
                    } else {
                        StatusCode::OK
                    },
                    Json(snapshot(id, status)),
                )
            }),
        ))
        .await;
        let error = server
            .client()
            .wait(id, WaitOptions::default())
            .await
            .unwrap_err();
        assert!(matches!(
            (status, error),
            ("failed", ClientError::RunFailed { .. })
                | ("aborted", ClientError::RunAborted { .. })
                | ("expired", ClientError::RunExpired { .. })
        ));
    }
}

#[tokio::test]
async fn structured_http_failures_are_distinct_from_failed_runs() {
    for status in [
        StatusCode::UNAUTHORIZED,
        StatusCode::CONFLICT,
        StatusCode::NOT_FOUND,
    ] {
        let id = Uuid::now_v7();
        let server = Server::new(Router::new().route(
            "/v1/llm/runs/{id}",
            get(move || async move { (status, Json(snapshot(id, "failed")["result"].clone())) }),
        ))
        .await;
        let client = server.client();
        match client.get_run(id).await.unwrap_err() {
            ClientError::Gateway {
                status: code,
                failure,
            } => {
                assert_eq!(code, status.as_u16());
                assert_eq!(failure.request_id, id);
                assert_eq!(failure.error.retry_after_ms, Some(1200));
            }
            error => panic!("{error}"),
        }
        assert!(
            matches!(client.wait(id, WaitOptions::default()).await.unwrap_err(),
            ClientError::WaitInterrupted { run_id, .. } if run_id == id)
        );
    }
}

#[tokio::test]
async fn response_validation_rejects_wrong_id_status_and_result_shapes() {
    for fault in [
        "id",
        "completion_id",
        "failure_id",
        "status",
        "result",
        "running_result",
        "missing_result",
        "json",
    ] {
        let id = Uuid::now_v7();
        let server = Server::new(Router::new().route(
            "/v1/llm/runs/{id}",
            get(move || async move {
                let mut value = snapshot(id, "succeeded");
                match fault {
                    "id" => value["run_id"] = json!(Uuid::now_v7()),
                    "completion_id" => value["result"]["request_id"] = json!(Uuid::now_v7()),
                    "failure_id" => {
                        value = snapshot(id, "failed");
                        value["result"]["request_id"] = json!(Uuid::now_v7());
                    }
                    "status" => return (StatusCode::GONE, Json(value)).into_response(),
                    "result" => value["result"] = json!({"unexpected": "result"}),
                    "running_result" => value["status"] = json!("running"),
                    "missing_result" => value["result"] = Value::Null,
                    "json" => return "secret invalid JSON".into_response(),
                    _ => unreachable!(),
                }
                Json(value).into_response()
            }),
        ))
        .await;
        assert!(
            matches!(
                server.client().get_run(id).await.unwrap_err(),
                ClientError::Protocol(_)
            ),
            "{fault}"
        );
    }
}

#[tokio::test]
async fn wait_timeout_covers_stalled_headers_body_and_polling_and_retains_id() {
    for phase in ["headers", "body", "running"] {
        let id = Uuid::now_v7();
        let count = Arc::new(AtomicUsize::new(0));
        let seen = count.clone();
        let server = Server::new(Router::new().route(
            "/v1/llm/runs/{id}",
            get(move || {
                let seen = seen.clone();
                async move {
                    seen.fetch_add(1, Ordering::SeqCst);
                    match phase {
                        "headers" => std::future::pending::<Response>().await,
                        "body" => {
                            Response::new(Body::from_stream(futures_util::stream::pending::<
                                Result<String, Infallible>,
                            >()))
                        }
                        _ => Json(snapshot(id, "running")).into_response(),
                    }
                }
            }),
        ))
        .await;
        let client = server.client();
        assert!(
            matches!(client.wait(id, WaitOptions::new(Duration::ZERO)).await.unwrap_err(), ClientError::WaitTimeout {run_id} if run_id == id)
        );
        assert_eq!(count.load(Ordering::SeqCst), 0);
        let error = tokio::time::timeout(
            Duration::from_secs(2),
            client.wait(id, WaitOptions::new(Duration::from_millis(100))),
        )
        .await
        .unwrap()
        .unwrap_err();
        assert!(matches!(error, ClientError::WaitTimeout {run_id} if run_id == id));
        assert_eq!(count.load(Ordering::SeqCst), 1);
    }
}

#[tokio::test]
async fn complete_retains_id_when_retrieval_fails_without_resubmitting() {
    let id = Uuid::now_v7();
    let count = Arc::new(AtomicUsize::new(0));
    let seen = count.clone();
    let server = Server::new(
        Router::new()
            .route(
                "/v1/llm/runs",
                post(move || {
                    let seen = seen.clone();
                    async move {
                        seen.fetch_add(1, Ordering::SeqCst);
                        (StatusCode::ACCEPTED, Json(snapshot(id, "running")))
                    }
                }),
            )
            .route(
                "/v1/llm/runs/{id}",
                get(|| async { StatusCode::BAD_GATEWAY }),
            ),
    )
    .await;
    assert!(
        matches!(server.client().complete(&key(), &input(), WaitOptions::default()).await.unwrap_err(),
        ClientError::WaitInterrupted { run_id, .. } if run_id == id)
    );
    assert_eq!(count.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn submission_does_not_retry_ambiguous_transport_failure() {
    let count = Arc::new(AtomicUsize::new(0));
    let seen = count.clone();
    let server = Server::new(Router::new().route(
        "/v1/llm/runs",
        post(move || {
            let seen = seen.clone();
            async move {
                seen.fetch_add(1, Ordering::SeqCst);
                Response::new(Body::from_stream(futures_util::stream::iter([Err::<
                    String,
                    _,
                >(
                    std::io::Error::other("response lost"),
                )])))
            }
        }),
    ))
    .await;
    assert!(matches!(
        server.client().submit(&key(), &input()).await.unwrap_err(),
        ClientError::Transport(_)
    ));
    assert_eq!(count.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn transport_timeout_and_streamed_size_limit_are_enforced() {
    for oversized in [true, false] {
        let server = Server::new(Router::new().route(
            "/v1/llm/runs/{id}",
            get(move || async move {
                if oversized {
                    Response::new(Body::from_stream(futures_util::stream::iter([Ok::<
                        _,
                        Infallible,
                    >(
                        "x".repeat(512),
                    )])))
                } else {
                    std::future::pending::<Response>().await
                }
            }),
        ))
        .await;
        let mut config = server.config();
        config.max_response_bytes = 100;
        config.request_timeout = Duration::from_millis(50);
        let error = LlmClient::new(config)
            .unwrap()
            .get_run(Uuid::now_v7())
            .await
            .unwrap_err();
        if oversized {
            assert!(matches!(error, ClientError::ResponseTooLarge));
        } else {
            assert!(matches!(error, ClientError::Transport(source) if source.is_timeout()));
        }
    }
}

#[tokio::test]
async fn redirects_and_unstructured_error_bodies_are_not_exposed() {
    let count = Arc::new(AtomicUsize::new(0));
    let seen = count.clone();
    let target = Server::new(Router::new().route(
        "/v1/llm/runs/{id}",
        get(move || {
            let seen = seen.clone();
            async move {
                seen.fetch_add(1, Ordering::SeqCst);
                StatusCode::OK
            }
        }),
    ))
    .await;
    let destination = format!("{}/v1/llm/runs/{}", target.url, Uuid::now_v7());
    let server = Server::new(Router::new().route(
        "/v1/llm/runs/{id}",
        get(move || {
            let destination = destination.clone();
            async move {
                (
                    StatusCode::TEMPORARY_REDIRECT,
                    [("location", destination)],
                    "private proxy content",
                )
            }
        }),
    ))
    .await;
    let error = server.client().get_run(Uuid::now_v7()).await.unwrap_err();
    assert!(matches!(error, ClientError::Http { status: 307, .. }));
    assert!(!format!("{error:?}").contains("private proxy content"));
    assert_eq!(count.load(Ordering::SeqCst), 0);
}

#[test]
fn keys_configuration_and_credentials_are_validated() {
    for value in [
        "".to_owned(),
        "x".repeat(257),
        "has space".into(),
        "π".into(),
        "newline\n".into(),
    ] {
        assert!(IdempotencyKey::new(&value).is_err());
        assert!(serde_json::from_value::<IdempotencyKey>(json!(value)).is_err());
    }
    assert!(IdempotencyKey::new("x".repeat(256)).is_ok());
    for url in [
        "http://localhost/",
        "ftp://example.com/",
        "https://secret@example.com/",
        "https://example.com/?secret=1",
        "https://example.com/#secret",
    ] {
        assert!(LlmClient::new(LlmClientConfig::new(url.parse().unwrap(), TOKEN)).is_err());
    }
    let client = LlmClient::new(LlmClientConfig::new(
        "https://example.com/".parse().unwrap(),
        TOKEN,
    ))
    .unwrap();
    assert!(!format!("{client:?}").contains(TOKEN));
}

#[tokio::test]
async fn abort_posts_authenticated_request_and_returns_actual_terminal_state() {
    for status in ["aborted", "succeeded", "failed", "expired"] {
        let id = Uuid::now_v7();
        let server = Server::new(Router::new().route(
            "/v1/llm/runs/{id}/abort",
            post(
                move |Path(actual): Path<Uuid>, headers: HeaderMap| async move {
                    assert_eq!(actual, id);
                    assert_eq!(headers["authorization"], format!("Bearer {TOKEN}"));
                    (
                        if status == "expired" {
                            StatusCode::GONE
                        } else {
                            StatusCode::OK
                        },
                        Json(snapshot(id, status)),
                    )
                },
            ),
        ))
        .await;
        let run = server.client().abort(id).await.unwrap();
        assert_eq!(run.run_id, id);
        assert_eq!(serde_json::to_value(run).unwrap()["status"], status);
    }
}
