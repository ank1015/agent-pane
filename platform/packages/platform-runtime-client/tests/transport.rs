use axum::{
    Router,
    body::{Body, Bytes},
    extract::State,
    http::{HeaderMap, Method, StatusCode, Uri},
    response::Response,
};
use platform_runtime_client::{
    ClientConfig, Command, Error, PlatformClient, RequestKey, WorkerRegistration, types::*,
};
use serde_json::{Value, json};
use std::{
    collections::VecDeque,
    sync::{Arc, Mutex},
    time::Duration,
};
use uuid::Uuid;

const TOKEN: &str = "worker-test-token-012345678901234567890";
const BOOT: &str = "bootstrap-test-token-01234567890123456789";
#[derive(Clone)]
struct Seen {
    method: Method,
    uri: Uri,
    headers: HeaderMap,
    body: Bytes,
}
struct Reply {
    status: StatusCode,
    body: Body,
    headers: HeaderMap,
}
impl Reply {
    fn json(status: u16, value: Value) -> Self {
        Self {
            status: StatusCode::from_u16(status).unwrap(),
            body: Body::from(value.to_string()),
            headers: HeaderMap::new(),
        }
    }
    fn header(mut self, name: &'static str, value: &str) -> Self {
        self.headers.insert(name, value.parse().unwrap());
        self
    }
}
#[derive(Clone)]
struct MockState {
    replies: Arc<Mutex<VecDeque<Reply>>>,
    seen: Arc<Mutex<Vec<Seen>>>,
}
struct Mock {
    url: String,
    state: MockState,
    task: tokio::task::JoinHandle<()>,
}
impl Drop for Mock {
    fn drop(&mut self) {
        self.task.abort();
    }
}
impl Mock {
    async fn new(replies: Vec<Reply>) -> Self {
        let state = MockState {
            replies: Arc::new(Mutex::new(replies.into())),
            seen: Arc::default(),
        };
        async fn handle(
            State(s): State<MockState>,
            method: Method,
            uri: Uri,
            headers: HeaderMap,
            body: Bytes,
        ) -> Response {
            s.seen.lock().unwrap().push(Seen {
                method,
                uri,
                headers,
                body,
            });
            let reply = s
                .replies
                .lock()
                .unwrap()
                .pop_front()
                .expect("unexpected extra request");
            let mut response = Response::new(reply.body);
            *response.status_mut() = reply.status;
            *response.headers_mut() = reply.headers;
            response
        }
        let router = Router::new().fallback(handle).with_state(state.clone());
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("http://{}", listener.local_addr().unwrap());
        let task = tokio::spawn(async move { axum::serve(listener, router).await.unwrap() });
        Self { url, state, task }
    }
    fn client(&self) -> PlatformClient {
        self.with_config(config())
    }
    fn with_config(&self, config: ClientConfig) -> PlatformClient {
        PlatformClient::new(&format!("{}/prefix", self.url), Uuid::nil(), TOKEN, config).unwrap()
    }
    fn seen(&self) -> Vec<Seen> {
        self.state.seen.lock().unwrap().clone()
    }
}
fn config() -> ClientConfig {
    ClientConfig {
        retry_base_delay: Duration::ZERO,
        retry_max_delay: Duration::ZERO,
        ..Default::default()
    }
}
fn command<T>(key: &str, body: T) -> Command<T> {
    Command::new(RequestKey::new(key).unwrap(), body)
}
fn empty_claim() -> Reply {
    Reply::json(200, json!({"items":[],"lease_duration_seconds":60}))
}
fn worker() -> Value {
    json!({"id":Uuid::nil(),"build_id":"v1","supported_harnesses":["test"],"capacity":1,"status":"accepting","started_at":"2026-09-04T00:00:00Z","last_seen_at":"2026-09-04T00:00:00Z"})
}

#[tokio::test]
async fn retry_keeps_exact_body_key_and_headers_and_persisted_commands_round_trip() {
    let mock = Mock::new(vec![
        Reply::json(
            503,
            json!({"error":{"code":"RUNTIME_BUSY","message":"retry"}}),
        ),
        empty_claim(),
        empty_claim(),
    ])
    .await;
    let client = mock.client();
    let original = command("allocation-1", Claim { limit: 2 });
    let saved = serde_json::to_string(&original).unwrap();
    let recovered: Command<Claim> = serde_json::from_str(&saved).unwrap();
    assert!(client.claim(&recovered).await.unwrap().items.is_empty());
    client
        .claim(&command("allocation-2", Claim { limit: 2 }))
        .await
        .unwrap();
    let seen = mock.seen();
    assert_eq!(seen.len(), 3);
    assert_eq!(seen[0].body, seen[1].body);
    assert_eq!(seen[0].headers, seen[1].headers);
    assert_eq!(seen[0].headers["idempotency-key"], "allocation-1");
    assert_eq!(seen[2].headers["idempotency-key"], "allocation-2");
    assert_eq!(seen[0].method, Method::POST);
    assert_eq!(
        seen[0].uri.path(),
        format!("/prefix/internal/workers/{}/claims", Uuid::nil())
    );
}

#[tokio::test]
async fn lost_response_is_retried_without_changing_run_epoch_or_payload() {
    let broken = Reply {
        status: StatusCode::OK,
        headers: HeaderMap::new(),
        body: Body::from_stream(futures_util::stream::iter(vec![Err::<Bytes, _>(
            std::io::Error::other("response lost"),
        )])),
    };
    let mock = Mock::new(vec![broken, Reply::json(201, json!({"items":[]}))]).await;
    let run_id = Uuid::new_v4();
    let run = mock
        .client()
        .run(Lease {
            run_id,
            lease_epoch: 7,
        })
        .unwrap();
    run.append_events(&command(
        "event-1",
        Events {
            events: vec![HarnessEvent {
                r#type: "model.delta".into(),
                payload: json!({"text":"hello"}).as_object().unwrap().clone(),
                occurred_at: None,
            }],
        },
    ))
    .await
    .unwrap();
    let seen = mock.seen();
    assert_eq!(seen.len(), 2);
    assert_eq!(seen[0].body, seen[1].body);
    assert_eq!(seen[0].headers, seen[1].headers);
    assert_eq!(seen[0].headers["x-lease-epoch"], "7");
    assert_eq!(seen[0].headers["x-worker-id"], Uuid::nil().to_string());
}

#[tokio::test]
async fn conflicts_and_auth_failures_are_not_retried_and_unknown_codes_survive() {
    for (status, code, known) in [
        (409, "LEASE_LOST", Some(ConflictCode::LeaseLost)),
        (
            409,
            "CHECKPOINT_VERSION_CONFLICT",
            Some(ConflictCode::CheckpointVersionConflict),
        ),
        (409, "FUTURE_CONFLICT", None),
        (401, "WORKER_UNAUTHORIZED", None),
    ] {
        let mock = Mock::new(vec![Reply::json(
            status,
            json!({"error":{"code":code,"message":"do not parse me"}}),
        )])
        .await;
        let error = mock
            .client()
            .claim(&command("claim", Claim { limit: 1 }))
            .await
            .err()
            .unwrap();
        assert_eq!(error.conflict(), known);
        match error {
            Error::Server(e) => assert_eq!(e.code(), Some(code)),
            _ => panic!("expected server error"),
        }
        assert_eq!(mock.seen().len(), 1);
    }
}

#[tokio::test]
async fn bootstrap_is_only_used_for_registration_and_debug_redacts_credentials() {
    let mock = Mock::new(vec![Reply::json(200, worker()), empty_claim()]).await;
    let client = mock.client();
    client
        .register(
            BOOT,
            &WorkerRegistration {
                build_id: "v1".into(),
                supported_harnesses: vec!["test".into()],
                capacity: 1,
            },
        )
        .await
        .unwrap();
    client
        .claim(&command("claim", Claim { limit: 1 }))
        .await
        .unwrap();
    let seen = mock.seen();
    assert_eq!(seen[0].headers["authorization"], format!("Bearer {BOOT}"));
    assert_eq!(seen[1].headers["authorization"], format!("Bearer {TOKEN}"));
    assert_eq!(
        serde_json::from_slice::<Value>(&seen[0].body).unwrap()["worker_token"],
        TOKEN
    );
    assert!(!format!("{client:?}").contains(TOKEN));
    assert!(!format!("{client:?}").contains(BOOT));
}

#[tokio::test]
async fn redirects_are_not_followed_and_patch_is_not_retried() {
    for reply in [
        Reply::json(307, json!({})).header("location", "http://127.0.0.1:1/steal"),
        Reply::json(503, json!({})),
    ] {
        let status = reply.status.as_u16();
        let mock = Mock::new(vec![reply]).await;
        let error = mock
            .client()
            .patch_worker(&PatchWorker {
                status: Some(WorkerStatus::Draining),
                capacity: None,
            })
            .await
            .err()
            .unwrap();
        assert!(matches!(error, Error::Server(e) if e.status == status));
        assert_eq!(mock.seen().len(), 1);
    }
}

#[tokio::test]
async fn bounded_retries_and_retry_after_return_control_to_worker() {
    let mock = Mock::new(vec![
        Reply::json(503, json!({})),
        Reply::json(503, json!({})),
        Reply::json(503, json!({})),
    ])
    .await;
    assert!(
        mock.client()
            .claim(&command("c", Claim { limit: 1 }))
            .await
            .is_err()
    );
    assert_eq!(mock.seen().len(), 3);
    let mock = Mock::new(vec![
        Reply::json(429, json!({})).header("retry-after", "120"),
    ])
    .await;
    assert!(
        mock.client()
            .claim(&command("c", Claim { limit: 1 }))
            .await
            .is_err()
    );
    assert_eq!(mock.seen().len(), 1);
}

#[tokio::test]
async fn response_size_and_invalid_json_are_bounded_protocol_failures() {
    let mock = Mock::new(vec![empty_claim()]).await;
    let client = mock.with_config(ClientConfig {
        max_response_bytes: 2,
        ..config()
    });
    assert!(matches!(
        client.claim(&command("c", Claim { limit: 1 })).await,
        Err(Error::Protocol(_))
    ));
    let mock = Mock::new(vec![Reply::json(200, json!({"wrong":"shape"}))]).await;
    assert!(matches!(
        mock.client().claim(&command("c", Claim { limit: 1 })).await,
        Err(Error::Protocol(_))
    ));
    assert_eq!(mock.seen().len(), 1);
}

#[tokio::test]
async fn reads_encode_queries_without_acknowledging_or_mutating_inputs() {
    let mock = Mock::new(vec![
        Reply::json(200, json!({"items":[],"next_after_sequence":null})),
        Reply::json(200, json!({"items":[]})),
    ])
    .await;
    let client = mock.client();
    client
        .run(Lease {
            run_id: Uuid::nil(),
            lease_epoch: 1,
        })
        .unwrap()
        .inputs(&SequenceQuery {
            after_sequence: Some(42),
            limit: Some(1),
            status: Some("pending".into()),
        })
        .await
        .unwrap();
    client.harnesses().await.unwrap();
    let seen = mock.seen();
    assert_eq!(seen[0].method, Method::GET);
    assert!(seen[0].body.is_empty());
    let query = seen[0].uri.query().unwrap();
    assert!(
        query.contains("after_sequence=42")
            && query.contains("status=pending")
            && query.contains("limit=1")
    );
    assert!(!seen[0].headers.contains_key("idempotency-key"));
}

#[test]
fn invalid_identity_and_unsafe_urls_are_rejected_locally() {
    for key in ["", "with space", "with\nnewline", "ü"] {
        assert!(RequestKey::new(key).is_err());
    }
    assert!(RequestKey::new("x".repeat(257)).is_err());
    assert!(
        serde_json::from_value::<Command<Claim>>(json!({"key":"bad key","body":{"limit":1}}))
            .is_err()
    );
    for url in [
        "http://example.com",
        "https://user:pass@example.com",
        "https://example.com?token=secret",
        "https://example.com/#fragment",
        "file:///tmp",
    ] {
        assert!(PlatformClient::new(url, Uuid::nil(), TOKEN, config()).is_err());
    }
    assert!(PlatformClient::new("http://[::1]:8000", Uuid::nil(), TOKEN, config()).is_ok());
}

#[tokio::test]
async fn parent_queries_share_read_retries_and_pagination_without_lease_headers() {
    let mock = Mock::new(vec![
        Reply::json(
            503,
            json!({"error":{"code":"RUNTIME_BUSY","message":"retry"}}),
        ),
        Reply::json(200, json!({"items":[],"next_after_revision":9})),
    ])
    .await;
    let parent = mock
        .client()
        .run(Lease {
            run_id: Uuid::new_v4(),
            lease_epoch: 7,
        })
        .unwrap();
    let child_session = Uuid::new_v4();
    let reads = parent.queries();
    let page = reads
        .session_messages(
            child_session,
            &MessageQuery {
                after_revision: Some(5),
                limit: Some(2),
                ..Default::default()
            },
        )
        .await
        .unwrap();
    assert_eq!(page.next_after_revision, Some(9));
    let seen = mock.seen();
    assert_eq!(seen.len(), 2);
    assert_eq!(seen[0].uri, seen[1].uri);
    for request in seen {
        assert_eq!(request.method, Method::GET);
        assert_eq!(
            request.uri.path(),
            format!("/prefix/api/sessions/{child_session}/messages")
        );
        assert!(request.uri.query().unwrap().contains("after_revision=5"));
        assert!(request.uri.query().unwrap().contains("limit=2"));
        assert!(request.body.is_empty());
        assert!(!request.headers.contains_key("x-worker-id"));
        assert!(!request.headers.contains_key("x-lease-epoch"));
        assert!(!request.headers.contains_key("idempotency-key"));
    }
}

#[tokio::test]
async fn harness_queries_reject_invalid_shared_ids_before_http() {
    let mock = Mock::new(vec![]).await;
    let reads = mock.client().queries();
    for id in ["", "Bad", "0bad", "a/b", "a%2fb", "aé", &"a".repeat(129)] {
        assert!(matches!(reads.harness(id).await, Err(Error::Invalid(_))));
    }
    assert!(mock.seen().is_empty());
}

#[tokio::test]
async fn oversized_requests_fail_before_network_io() {
    let mock = Mock::new(vec![]).await;
    let run = mock
        .client()
        .run(Lease {
            run_id: Uuid::nil(),
            lease_epoch: 1,
        })
        .unwrap();
    let request = command(
        "large-event",
        Events {
            events: vec![HarnessEvent {
                r#type: "harness.data".into(),
                payload: json!({"data":"x".repeat(1024 * 1024)})
                    .as_object()
                    .unwrap()
                    .clone(),
                occurred_at: None,
            }],
        },
    );
    assert!(matches!(
        run.append_events(&request).await,
        Err(Error::Invalid(_))
    ));
    assert!(mock.seen().is_empty());
}

#[tokio::test]
async fn body_timeouts_are_bounded_and_keep_request_identity() {
    fn delayed() -> Reply {
        Reply {
            status: StatusCode::OK,
            headers: HeaderMap::new(),
            body: Body::from_stream(futures_util::stream::once(async {
                tokio::time::sleep(Duration::from_millis(250)).await;
                Ok::<_, std::io::Error>(Bytes::from_static(b"{}"))
            })),
        }
    }
    let mock = Mock::new(vec![delayed(), delayed()]).await;
    let client = mock.with_config(ClientConfig {
        request_timeout: Duration::from_millis(30),
        max_attempts: 2,
        ..config()
    });
    let error = client
        .claim(&command("timeout", Claim { limit: 1 }))
        .await
        .err()
        .unwrap();
    assert!(matches!(error, Error::Transport { timed_out: true }));
    let seen = mock.seen();
    assert_eq!(seen.len(), 2);
    assert_eq!(seen[0].headers, seen[1].headers);
    assert_eq!(seen[0].body, seen[1].body);
}
