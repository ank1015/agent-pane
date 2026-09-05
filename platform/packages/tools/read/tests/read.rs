use axum::{Json, Router, extract::Path, http::HeaderMap, routing::post};
use execution_client::{ExecutionClient, ExecutionClientConfig, GatewayHostRuntime};
use execution_core::*;
use execution_supervisor_core::{
    SupervisorConfig, SupervisorLimits, SupervisorRoot, SupervisorRuntime,
};
use execution_wire::{
    Operation, OperationResult, RequestEnvelope, ResponseEnvelope, dispatch_request,
};
use std::{
    sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    },
    time::Duration,
};
use tool_read::{ReadConfig, ReadInput, ReadTool, Truncation};

struct Host {
    _temp: tempfile::TempDir,
    runtime: GatewayHostRuntime,
    supervisor: Arc<SupervisorRuntime>,
    calls: Arc<AtomicUsize>,
    server: tokio::task::JoinHandle<()>,
}
impl Drop for Host {
    fn drop(&mut self) {
        self.server.abort();
    }
}

impl Host {
    async fn new(content: &[u8], fault: &str) -> Self {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().join("workspace");
        std::fs::create_dir_all(root.join("project/subdir")).unwrap();
        std::fs::write(root.join("project/file.txt"), content).unwrap();
        let supervisor = Arc::new(
            SupervisorRuntime::new(SupervisorConfig {
                host_id: ExecutionHostId::generate(),
                state_directory: temp.path().join("state"),
                roots: vec![SupervisorRoot {
                    id: RootId::new("work").unwrap(),
                    name: "Work".into(),
                    path: root,
                    read_only: false,
                }],
                limits: SupervisorLimits {
                    max_read_bytes: if fault == "small-limit" {
                        3
                    } else {
                        SupervisorLimits::default().max_read_bytes
                    },
                    ..Default::default()
                },
            })
            .await
            .unwrap(),
        );
        let calls = Arc::new(AtomicUsize::new(0));
        let count = calls.clone();
        let target = supervisor.clone();
        let fault = fault.to_owned();
        let app = Router::new().route(
            "/v1/hosts/{host}/operations",
            post(
                move |Path(id): Path<String>,
                      headers: HeaderMap,
                      Json(request): Json<RequestEnvelope>| {
                    let target = target.clone();
                    let count = count.clone();
                    let fault = fault.clone();
                    async move {
                        assert_eq!(headers["authorization"], "Bearer read-tool-test-token");
                        assert_eq!(id, target.descriptor().host_id.as_str());
                        count.fetch_add(1, Ordering::SeqCst);
                        if matches!(request.operation, Operation::FilesystemRead(_))
                            && fault == "stall"
                        {
                            return std::future::pending::<Json<ResponseEnvelope>>().await;
                        }
                        let mut reply =
                            dispatch_request(target.as_ref(), &OperationContext::new(), request)
                                .await;
                        if let ResponseEnvelope::Success {
                            result: OperationResult::ReadFile(value),
                            ..
                        } = &mut reply
                        {
                            if fault == "revision" && value.offset > 0 {
                                value.metadata.revision =
                                    Some(FileRevision::new("changed").unwrap());
                            }
                            if fault == "offset" {
                                value.offset += 1;
                            }
                            if fault == "path" {
                                value.metadata.path.path = "different.txt".into();
                            }
                            if fault == "empty" {
                                value.data = BinaryData::default();
                                value.eof = false;
                            }
                            if fault == "eof" {
                                value.eof = true;
                            }
                        }
                        Json(reply)
                    }
                },
            ),
        );
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("http://{}", listener.local_addr().unwrap());
        let server = tokio::spawn(async move {
            axum::serve(listener, app).await.unwrap();
        });
        let mut config = ExecutionClientConfig::new(url.parse().unwrap(), "read-tool-test-token");
        config.allow_insecure_http = true;
        let runtime = ExecutionClient::new(config)
            .unwrap()
            .connect_host(
                &OperationContext::new(),
                supervisor.descriptor().host_id.clone(),
            )
            .await
            .unwrap();
        Self {
            _temp: temp,
            runtime,
            supervisor,
            calls,
            server,
        }
    }
    fn tool(&self, config: ReadConfig) -> ReadTool<'_> {
        ReadTool::new(
            &self.runtime,
            ExecutionPath::new(RootId::new("work").unwrap(), "project").unwrap(),
            config,
        )
        .unwrap()
    }
}
fn input(offset: Option<u64>, limit: Option<u64>) -> ReadInput {
    ReadInput {
        file_path: "file.txt".into(),
        offset,
        limit,
    }
}
fn context() -> OperationContext {
    OperationContext::with_timeout(Duration::from_secs(5))
}

#[tokio::test]
async fn remote_window_preserves_unicode_crlf_and_revision() {
    let host = Host::new("one\r\n\tπ🙂\r\nlast".as_bytes(), "").await;
    let tool = host.tool(ReadConfig {
        chunk_bytes: 2,
        ..Default::default()
    });
    let result = tool
        .execute(&context(), input(Some(2), Some(1)))
        .await
        .unwrap();
    assert_eq!(result.content, "\tπ🙂\r\n");
    assert_eq!(result.start_line, 2);
    assert_eq!(result.end_line, Some(2));
    assert_eq!(result.next_offset, Some(3));
    assert_eq!(result.truncation, Some(Truncation::LineLimit));
    assert!(!result.eof);
    assert!(result.to_text().starts_with("     2\t\tπ🙂\r\n"));
    assert_eq!(result.path.path, "project/file.txt");
    let next = tool
        .execute(&context(), input(result.next_offset, None))
        .await
        .unwrap();
    assert_eq!(next.content, "last");
    assert!(next.eof);
    assert_eq!(next.end_line, Some(3));
    assert_eq!(next.revision, result.revision);
    let json = serde_json::to_value(next).unwrap();
    assert_eq!(json["content"], "last");
}

#[tokio::test]
async fn empty_files_and_line_boundaries_have_precise_eof() {
    for (text, count) in [
        ("", None),
        ("\n", Some(1)),
        ("a\n", Some(1)),
        ("a\n\n", Some(2)),
        ("a\nb", Some(2)),
    ] {
        let host = Host::new(text.as_bytes(), "").await;
        let tool = host.tool(ReadConfig {
            chunk_bytes: 1,
            ..Default::default()
        });
        let result = tool.execute(&context(), input(None, count)).await.unwrap();
        assert_eq!(result.content, text);
        assert_eq!(result.end_line, count);
        assert!(result.eof);
        assert_eq!(result.next_offset, None);
        assert_eq!(result.truncation, None);
        let error = tool
            .execute(&context(), input(Some(count.unwrap_or(0) + 2), None))
            .await
            .unwrap_err();
        assert_eq!(error.code, ExecutionErrorCode::InvalidRequest);
    }
}

#[tokio::test]
async fn byte_limit_returns_whole_lines_and_never_silently_clips() {
    let host = Host::new(format!("short\n{}\nlast", "a".repeat(400)).as_bytes(), "").await;
    let tool = host.tool(ReadConfig {
        max_output_bytes: 350,
        chunk_bytes: 7,
        ..Default::default()
    });
    let result = tool.execute(&context(), input(None, None)).await.unwrap();
    assert_eq!(result.content, "short\n");
    assert_eq!(result.next_offset, Some(2));
    assert_eq!(result.truncation, Some(Truncation::ByteLimit));
    assert!(result.to_text().len() <= 350);
    let error = tool
        .execute(&context(), input(Some(2), None))
        .await
        .unwrap_err();
    assert_eq!(error.code, ExecutionErrorCode::ResourceExhausted);
    assert!(error.message.contains("line 2"));
    let result = tool
        .execute(&context(), input(Some(3), None))
        .await
        .unwrap();
    assert_eq!(result.content, "last");
}

#[tokio::test]
async fn default_and_explicit_line_limits_share_the_harness_cap() {
    let host = Host::new(b"a\nb\nc\n", "").await;
    let tool = host.tool(ReadConfig {
        max_lines: 2,
        ..Default::default()
    });
    for requested in [None, Some(2), Some(5000)] {
        let result = tool
            .execute(&context(), input(None, requested))
            .await
            .unwrap();
        assert_eq!(result.content, "a\nb\n");
        assert_eq!(result.next_offset, Some(3));
    }
}

#[tokio::test]
async fn chunks_respect_the_remote_hosts_advertised_limit() {
    let host = Host::new(b"first\nsecond\n", "small-limit").await;
    let result = host
        .tool(ReadConfig::default())
        .execute(&context(), input(None, None))
        .await
        .unwrap();
    assert_eq!(result.content, "first\nsecond\n");
    assert!(result.eof);
    assert!(host.calls.load(Ordering::SeqCst) >= 7); // describe, stat, >=5 reads
}

#[tokio::test]
async fn missing_directories_binary_and_non_utf8_are_clear_errors() {
    for (bytes, code) in [
        (b"hello\0world".as_slice(), ExecutionErrorCode::Unsupported),
        (&[0xff, b'\n'], ExecutionErrorCode::Unsupported),
    ] {
        let host = Host::new(bytes, "").await;
        assert_eq!(
            host.tool(ReadConfig::default())
                .execute(&context(), input(None, None))
                .await
                .unwrap_err()
                .code,
            code
        );
    }
    let host = Host::new(b"ok", "").await;
    let tool = host.tool(ReadConfig::default());
    for (path, code) in [
        ("missing", ExecutionErrorCode::NotFound),
        ("subdir", ExecutionErrorCode::IsDirectory),
    ] {
        let mut request = input(None, None);
        request.file_path = path.into();
        assert_eq!(
            tool.execute(&context(), request).await.unwrap_err().code,
            code
        );
    }
}

#[tokio::test]
async fn revision_changes_and_malformed_chunks_do_not_return_mixed_content() {
    for (fault, code) in [
        ("revision", ExecutionErrorCode::RevisionConflict),
        ("offset", ExecutionErrorCode::Internal),
        ("path", ExecutionErrorCode::Internal),
        ("empty", ExecutionErrorCode::Internal),
        ("eof", ExecutionErrorCode::RevisionConflict),
    ] {
        let host = Host::new(b"first\nsecond\n", fault).await;
        let error = host
            .tool(ReadConfig {
                chunk_bytes: 3,
                ..Default::default()
            })
            .execute(&context(), input(None, None))
            .await
            .unwrap_err();
        assert_eq!(error.code, code, "{fault}");
    }
}

#[tokio::test]
async fn absolute_paths_map_to_the_remote_root_and_escapes_fail() {
    let host = Host::new(b"remote", "").await;
    let tool = host.tool(ReadConfig::default());
    let root = &host.runtime.descriptor().roots[0].native_path;
    let mut request = input(None, None);
    request.file_path = format!("{root}/project/file.txt");
    assert_eq!(
        tool.execute(&context(), request).await.unwrap().content,
        "remote"
    );
    for path in [
        format!("{root}-other/file.txt"),
        "/outside/file.txt".into(),
        "../file.txt".into(),
    ] {
        let before = host.calls.load(Ordering::SeqCst);
        let mut request = input(None, None);
        request.file_path = path;
        assert!(tool.execute(&context(), request).await.is_err());
        assert_eq!(host.calls.load(Ordering::SeqCst), before);
    }
}

#[cfg(unix)]
#[tokio::test]
async fn supervisor_enforces_symlink_containment() {
    let host = Host::new(b"inside", "").await;
    let root = &host.runtime.descriptor().roots[0].native_path;
    let outside = host._temp.path().join("outside.txt");
    std::fs::write(&outside, "outside").unwrap();
    std::os::unix::fs::symlink(outside, format!("{root}/project/escape")).unwrap();
    let mut request = input(None, None);
    request.file_path = "escape".into();
    assert_eq!(
        host.tool(ReadConfig::default())
            .execute(&context(), request)
            .await
            .unwrap_err()
            .code,
        ExecutionErrorCode::PathOutsideRoot
    );
    host.supervisor.shutdown().await;
}

#[tokio::test]
async fn scan_limit_and_cancellation_bound_work() {
    let host = Host::new(b"01234567890123456789\nnext", "").await;
    let error = host
        .tool(ReadConfig {
            max_scan_bytes: 8,
            chunk_bytes: 3,
            ..Default::default()
        })
        .execute(&context(), input(Some(2), None))
        .await
        .unwrap_err();
    assert_eq!(error.code, ExecutionErrorCode::ResourceExhausted);
    let cancelled = context();
    cancelled.cancel();
    let before = host.calls.load(Ordering::SeqCst);
    assert_eq!(
        host.tool(ReadConfig::default())
            .execute(&cancelled, input(None, None))
            .await
            .unwrap_err()
            .code,
        ExecutionErrorCode::Cancelled
    );
    assert_eq!(host.calls.load(Ordering::SeqCst), before);
    let host = Host::new(b"text", "stall").await;
    let context = OperationContext::with_timeout(Duration::from_millis(100));
    let error = tokio::time::timeout(
        Duration::from_secs(2),
        host.tool(ReadConfig::default())
            .execute(&context, input(None, None)),
    )
    .await
    .unwrap()
    .unwrap_err();
    assert_eq!(error.code, ExecutionErrorCode::DeadlineExceeded);
}

#[tokio::test]
async fn schema_and_runtime_reject_invalid_model_arguments() {
    let host = Host::new(b"ok", "").await;
    let tool = host.tool(ReadConfig::default());
    let schema = jsonschema::validator_for(&tool.input_schema()).unwrap();
    assert!(schema.is_valid(&serde_json::json!({"file_path":"file.txt"})));
    for value in [
        serde_json::json!({"file_path":"file.txt", "offset":0}),
        serde_json::json!({"file_path":"file.txt", "limit":1.5}),
        serde_json::json!({"file_path":"file.txt", "machine_id":"ignored"}),
        serde_json::json!({"file_path":""}),
    ] {
        assert!(!schema.is_valid(&value));
    }
    assert!(
        serde_json::from_value::<ReadInput>(
            serde_json::json!({"file_path":"file.txt", "extra":true})
        )
        .is_err()
    );
    let before = host.calls.load(Ordering::SeqCst);
    for request in [input(Some(0), None), input(None, Some(0))] {
        assert_eq!(
            tool.execute(&context(), request).await.unwrap_err().code,
            ExecutionErrorCode::InvalidRequest
        );
    }
    assert_eq!(before, host.calls.load(Ordering::SeqCst));
}
