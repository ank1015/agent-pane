use std::{
    convert::Infallible,
    sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    },
    time::Duration,
};

use axum::{
    Json, Router,
    body::Body,
    extract::Path,
    handler::Handler,
    http::{HeaderMap, StatusCode},
    response::{IntoResponse, Response},
    routing::post,
};
use execution_client::{ExecutionClient, ExecutionClientConfig};
use execution_conformance::{ConformanceConfig, run_all};
use execution_core::{
    BinaryData, CreateDirectoryRequest, ExecutionError, ExecutionErrorCode, ExecutionFeatures,
    ExecutionHostDescriptor, ExecutionHostId, ExecutionLimits, ExecutionPath, ExecutionRoot,
    ExecutionRuntime, OperatingSystem, OperationContext, OperationId, PathConvention, RootId,
    StatRequest, SupervisorGenerationId, WriteCondition, WriteFileRequest,
};
use execution_supervisor_core::{
    SupervisorConfig, SupervisorLimits, SupervisorRoot, SupervisorRuntime,
};
use execution_wire::{
    Operation, OperationResult, RequestEnvelope, ResponseEnvelope, dispatch_request,
};
use tokio::{net::TcpListener, sync::Notify, task::JoinHandle};
use url::Url;

const TOKEN: &str = "private-client-test-token";

struct Server {
    base_url: Url,
    task: JoinHandle<()>,
}

impl Server {
    async fn start<H, T>(handler: H) -> Self
    where
        H: Handler<T, ()>,
        T: 'static,
    {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let base_url = format!("http://{}/gateway/", listener.local_addr().unwrap())
            .parse()
            .unwrap();
        let app = Router::new().route("/gateway/v1/hosts/{host_id}/operations", post(handler));
        let task = tokio::spawn(async move {
            axum::serve(listener, app).await.unwrap();
        });
        Self { base_url, task }
    }

    fn config(&self) -> ExecutionClientConfig {
        let mut config = ExecutionClientConfig::new(self.base_url.clone(), TOKEN);
        config.allow_insecure_http = true;
        config
    }

    fn client(&self) -> ExecutionClient {
        ExecutionClient::new(self.config()).unwrap()
    }
}

impl Drop for Server {
    fn drop(&mut self) {
        self.task.abort();
    }
}

fn descriptor(host_id: ExecutionHostId) -> ExecutionHostDescriptor {
    ExecutionHostDescriptor {
        host_id,
        supervisor_generation_id: SupervisorGenerationId::generate(),
        operating_system: OperatingSystem::Linux,
        architecture: "test".into(),
        path_convention: PathConvention::Unix,
        roots: vec![ExecutionRoot {
            id: RootId::new("workspace").unwrap(),
            name: "Workspace".into(),
            native_path: "/workspace".into(),
            read_only: false,
        }],
        features: ExecutionFeatures {
            pty: true,
            process_signals: true,
            file_revisions: true,
        },
        limits: ExecutionLimits::default(),
    }
}

fn describe_response(request: RequestEnvelope, host_id: &str) -> Response {
    Json(ResponseEnvelope::success(
        request.request_id,
        OperationResult::HostDescriptor(descriptor(host_id.parse().unwrap())),
    ))
    .into_response()
}

fn stat() -> StatRequest {
    StatRequest {
        path: ExecutionPath::root(RootId::new("workspace").unwrap()),
        follow_symlinks: true,
    }
}

#[tokio::test]
async fn complete_runtime_conformance_through_authenticated_http() {
    let temporary = tempfile::tempdir().unwrap();
    let root = temporary.path().join("workspace");
    tokio::fs::create_dir(&root).await.unwrap();
    let supervisor = Arc::new(
        SupervisorRuntime::new(SupervisorConfig {
            host_id: ExecutionHostId::generate(),
            state_directory: temporary.path().join("state"),
            roots: vec![SupervisorRoot {
                id: RootId::new("workspace").unwrap(),
                name: "Workspace".into(),
                path: root,
                read_only: false,
            }],
            limits: SupervisorLimits::default(),
        })
        .await
        .unwrap(),
    );
    let target = supervisor.clone();
    let server = Server::start(
        move |Path(host_id): Path<String>,
              headers: HeaderMap,
              Json(request): Json<RequestEnvelope>| {
            let target = target.clone();
            async move {
                assert_eq!(headers["authorization"], format!("Bearer {TOKEN}"));
                assert_eq!(host_id, target.descriptor().host_id.as_str());
                Json(dispatch_request(target.as_ref(), &OperationContext::new(), request).await)
            }
        },
    )
    .await;
    let host = server
        .client()
        .connect_host(
            &OperationContext::new(),
            supervisor.descriptor().host_id.clone(),
        )
        .await
        .unwrap();
    assert_eq!(host.descriptor(), supervisor.descriptor());
    host.filesystem()
        .create_directory(
            &OperationContext::new(),
            CreateDirectoryRequest {
                operation_id: OperationId::generate(),
                path: ExecutionPath::new(RootId::new("workspace").unwrap(), "created-by-client")
                    .unwrap(),
                recursive: false,
            },
        )
        .await
        .unwrap();
    let result = run_all(&host, &ConformanceConfig::for_runtime(&host).unwrap()).await;
    supervisor.shutdown().await;
    let report = result.unwrap();
    assert!(report.passed_checks().len() >= 9);
    assert!(report.skipped_checks().is_empty(), "{report}");
}

#[tokio::test]
async fn handshake_rejects_wrong_identity_invalid_descriptor_and_protocol_faults() {
    for fault in [
        "version",
        "correlation",
        "result",
        "identity",
        "descriptor",
        "json",
    ] {
        let server = Server::start(
            move |Path(host_id): Path<String>, Json(request): Json<RequestEnvelope>| async move {
                let mut descriptor = descriptor(host_id.parse().unwrap());
                if fault == "identity" {
                    descriptor.host_id = ExecutionHostId::generate();
                }
                if fault == "descriptor" {
                    descriptor.roots.clear();
                }
                let mut response = serde_json::to_value(ResponseEnvelope::success(
                    request.request_id,
                    OperationResult::HostDescriptor(descriptor),
                ))
                .unwrap();
                match fault {
                    "version" => response["version"] = 99.into(),
                    "correlation" => response["request_id"] = "another-request".into(),
                    "result" => response["result"] = serde_json::json!({"result": "unit"}),
                    "json" => return "not JSON".into_response(),
                    _ => {}
                }
                Json(response).into_response()
            },
        )
        .await;
        let error = server
            .client()
            .connect_host(&OperationContext::new(), ExecutionHostId::generate())
            .await
            .unwrap_err();
        assert_eq!(error.code, ExecutionErrorCode::Internal, "{fault}");
        assert_eq!(error.details["source"], "protocol");
    }
}

#[tokio::test]
async fn gateway_errors_preserve_codes_retryability_and_diagnostics() {
    for (status, gateway_code, expected) in [
        (
            StatusCode::CONFLICT,
            "HOST_NOT_READY",
            ExecutionErrorCode::Unavailable,
        ),
        (
            StatusCode::NOT_FOUND,
            "NOT_FOUND",
            ExecutionErrorCode::NotFound,
        ),
        (
            StatusCode::UNAUTHORIZED,
            "UNAUTHORIZED",
            ExecutionErrorCode::PermissionDenied,
        ),
        (
            StatusCode::TOO_MANY_REQUESTS,
            "HOST_BUSY",
            ExecutionErrorCode::ResourceExhausted,
        ),
        (
            StatusCode::GATEWAY_TIMEOUT,
            "HOST_OPERATION_TIMEOUT",
            ExecutionErrorCode::DeadlineExceeded,
        ),
        (
            StatusCode::SERVICE_UNAVAILABLE,
            "HOST_DISCONNECTED",
            ExecutionErrorCode::Unavailable,
        ),
    ] {
        let server = Server::start(move || async move {
            (status, [("x-request-id", "gateway-trace")], Json(serde_json::json!({
                "error": { "code": gateway_code, "message": "host routing failed", "retryable": true,
                    "details": {"reason": "test"} }
            })))
        }).await;
        let error = server
            .client()
            .connect_host(&OperationContext::new(), ExecutionHostId::generate())
            .await
            .unwrap_err();
        assert_eq!(error.code, expected);
        assert!(error.retryable);
        assert_eq!(error.details["gateway_code"], gateway_code);
        assert_eq!(error.details["gateway_details"]["reason"], "test");
        assert_eq!(error.details["gateway_request_id"], "gateway-trace");
        assert_eq!(error.details["http_status"], status.as_u16());
    }
}

#[tokio::test]
async fn runtime_errors_are_unchanged_and_success_variants_are_checked() {
    for wrong_variant in [false, true] {
        let expected = ExecutionError::new(ExecutionErrorCode::RevisionConflict, "file changed")
            .retryable(true)
            .with_detail("revision", "current");
        let remote = expected.clone();
        let server = Server::start(
            move |Path(host_id): Path<String>, Json(request): Json<RequestEnvelope>| {
                let remote = remote.clone();
                async move {
                    if matches!(request.operation, Operation::Describe) {
                        return describe_response(request, &host_id);
                    }
                    Json(if wrong_variant {
                        ResponseEnvelope::success(request.request_id, OperationResult::Unit)
                    } else {
                        ResponseEnvelope::error(request.request_id, remote)
                    })
                    .into_response()
                }
            },
        )
        .await;
        let host = server
            .client()
            .connect_host(&OperationContext::new(), ExecutionHostId::generate())
            .await
            .unwrap();
        let error = host
            .filesystem()
            .stat(&OperationContext::new(), stat())
            .await
            .unwrap_err();
        if wrong_variant {
            assert_eq!(error.details["source"], "protocol");
        } else {
            assert_eq!(error, expected);
        }
    }
}

#[tokio::test]
async fn cancellation_and_deadlines_cover_response_headers_and_streaming_body() {
    for stalled_body in [false, true] {
        for cancel in [false, true] {
            let received = Arc::new(Notify::new());
            let signal = received.clone();
            let server = Server::start(move || {
                let signal = signal.clone();
                async move {
                    signal.notify_one();
                    if stalled_body {
                        let stream = futures_util::stream::pending::<Result<String, Infallible>>();
                        Response::new(Body::from_stream(stream))
                    } else {
                        std::future::pending::<Response>().await
                    }
                }
            })
            .await;
            let context = if cancel {
                OperationContext::new()
            } else {
                OperationContext::with_timeout(Duration::from_millis(100))
            };
            let worker_context = context.clone();
            let client = server.client();
            let pending = tokio::spawn(async move {
                client
                    .connect_host(&worker_context, ExecutionHostId::generate())
                    .await
            });
            tokio::time::timeout(Duration::from_secs(2), received.notified())
                .await
                .unwrap();
            if cancel {
                context.cancel();
            }
            let error = tokio::time::timeout(Duration::from_secs(2), pending)
                .await
                .unwrap()
                .unwrap()
                .unwrap_err();
            assert_eq!(
                error.code,
                if cancel {
                    ExecutionErrorCode::Cancelled
                } else {
                    ExecutionErrorCode::DeadlineExceeded
                }
            );
        }
    }
}

#[tokio::test]
async fn configured_timeout_applies_without_a_context_deadline() {
    let server = Server::start(|| async { std::future::pending::<Response>().await }).await;
    let mut config = server.config();
    config.request_timeout = Duration::from_millis(50);
    let error = tokio::time::timeout(
        Duration::from_secs(2),
        ExecutionClient::new(config)
            .unwrap()
            .connect_host(&OperationContext::new(), ExecutionHostId::generate()),
    )
    .await
    .unwrap()
    .unwrap_err();
    assert_eq!(error.code, ExecutionErrorCode::DeadlineExceeded);
}

#[tokio::test]
async fn response_limit_covers_declared_and_streamed_bodies() {
    for streamed in [false, true] {
        let server = Server::start(move || async move {
            if streamed {
                Response::new(Body::from_stream(futures_util::stream::iter([
                    Ok::<_, Infallible>("x".repeat(128)),
                    Ok("y".repeat(128)),
                ])))
            } else {
                "x".repeat(256).into_response()
            }
        })
        .await;
        let mut config = server.config();
        config.max_response_bytes = 200;
        let error = ExecutionClient::new(config)
            .unwrap()
            .connect_host(&OperationContext::new(), ExecutionHostId::generate())
            .await
            .unwrap_err();
        assert_eq!(error.code, ExecutionErrorCode::ResourceExhausted);
    }
}

#[tokio::test]
async fn failed_mutation_is_sent_once_with_unchanged_identity_and_payload() {
    let count = Arc::new(AtomicUsize::new(0));
    let seen = count.clone();
    let mutation = WriteFileRequest {
        operation_id: OperationId::generate(),
        path: stat().path,
        data: BinaryData::new(vec![0, 255, 1]),
        condition: WriteCondition::MustNotExist,
        create_parents: false,
        follow_symlinks: false,
    };
    let expected = mutation.clone();
    let server = Server::start(
        move |Path(host_id): Path<String>, Json(request): Json<RequestEnvelope>| {
            let seen = seen.clone();
            let expected = expected.clone();
            async move {
                if matches!(request.operation, Operation::Describe) {
                    return describe_response(request, &host_id);
                }
                assert_eq!(request.operation, Operation::FilesystemWrite(expected));
                seen.fetch_add(1, Ordering::SeqCst);
                // Simulate a connection ending after the gateway accepted a mutation.
                Response::new(Body::from_stream(futures_util::stream::iter([Err::<
                    String,
                    _,
                >(
                    std::io::Error::other("response lost"),
                )])))
            }
        },
    )
    .await;
    let host = server
        .client()
        .connect_host(&OperationContext::new(), ExecutionHostId::generate())
        .await
        .unwrap();
    assert!(
        host.filesystem()
            .write(&OperationContext::new(), mutation)
            .await
            .is_err()
    );
    assert_eq!(count.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn no_network_for_cancelled_expired_or_invalid_requests() {
    let count = Arc::new(AtomicUsize::new(0));
    let seen = count.clone();
    let server = Server::start(
        move |Path(host_id): Path<String>, Json(request): Json<RequestEnvelope>| {
            let seen = seen.clone();
            async move {
                seen.fetch_add(1, Ordering::SeqCst);
                describe_response(request, &host_id)
            }
        },
    )
    .await;
    let client = server.client();
    let cancelled = OperationContext::new();
    cancelled.cancel();
    assert_eq!(
        client
            .connect_host(&cancelled, ExecutionHostId::generate())
            .await
            .unwrap_err()
            .code,
        ExecutionErrorCode::Cancelled
    );
    assert_eq!(
        client
            .connect_host(
                &OperationContext::with_timeout(Duration::ZERO),
                ExecutionHostId::generate()
            )
            .await
            .unwrap_err()
            .code,
        ExecutionErrorCode::DeadlineExceeded
    );
    assert_eq!(
        client
            .connect_host(
                &OperationContext::new(),
                ExecutionHostId::new("not-a-uuid").unwrap()
            )
            .await
            .unwrap_err()
            .code,
        ExecutionErrorCode::InvalidRequest
    );
    assert_eq!(count.load(Ordering::SeqCst), 0);
    let host = client
        .connect_host(&OperationContext::new(), ExecutionHostId::generate())
        .await
        .unwrap();
    let mut request = stat();
    request.path.path = "../escape".into();
    assert_eq!(
        host.filesystem()
            .stat(&OperationContext::new(), request)
            .await
            .unwrap_err()
            .code,
        ExecutionErrorCode::InvalidRequest
    );
    assert_eq!(count.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn redirects_are_not_followed_and_proxy_error_bodies_are_not_exposed() {
    let received = Arc::new(AtomicUsize::new(0));
    let seen = received.clone();
    let destination = Server::start(move || {
        let seen = seen.clone();
        async move {
            seen.fetch_add(1, Ordering::SeqCst);
            StatusCode::OK
        }
    })
    .await;
    let destination_url = destination
        .base_url
        .join(&format!(
            "v1/hosts/{}/operations",
            ExecutionHostId::generate()
        ))
        .unwrap();
    let server = Server::start(move || {
        let url = destination_url.to_string();
        async move {
            (
                StatusCode::TEMPORARY_REDIRECT,
                [("location", url)],
                "sensitive proxy response",
            )
        }
    })
    .await;
    let error = server
        .client()
        .connect_host(&OperationContext::new(), ExecutionHostId::generate())
        .await
        .unwrap_err();
    assert_eq!(error.details["http_status"], 307);
    assert!(!format!("{error:?}").contains("sensitive proxy response"));
    assert_eq!(received.load(Ordering::SeqCst), 0);
}

#[test]
fn configuration_rejects_unsafe_urls_and_invalid_limits_without_exposing_tokens() {
    for url in [
        "http://localhost/",
        "ftp://example.com/",
        "https://user:secret@example.com/",
        "https://example.com/?token=secret",
        "https://example.com/#secret",
    ] {
        let error = ExecutionClient::new(ExecutionClientConfig::new(url.parse().unwrap(), TOKEN))
            .unwrap_err();
        assert!(!format!("{error:?}").contains(TOKEN));
        assert!(!format!("{error:?}").contains("secret"));
    }
    for token in ["", "two words", "newline\n", "non-ascii-π"] {
        assert!(
            ExecutionClient::new(ExecutionClientConfig::new(
                "https://example.com/".parse().unwrap(),
                token
            ))
            .is_err()
        );
    }
    let mut config = ExecutionClientConfig::new("https://example.com/".parse().unwrap(), TOKEN);
    config.request_timeout = Duration::ZERO;
    assert!(ExecutionClient::new(config).is_err());
    let mut config = ExecutionClientConfig::new("https://example.com/".parse().unwrap(), TOKEN);
    config.max_response_bytes = 0;
    assert!(ExecutionClient::new(config).is_err());
    let client = ExecutionClient::new(ExecutionClientConfig::new(
        "https://example.com/".parse().unwrap(),
        TOKEN,
    ))
    .unwrap();
    assert!(!format!("{client:?}").contains(TOKEN));
}
