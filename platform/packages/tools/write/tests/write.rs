use axum::{
    Json, Router,
    extract::Path,
    http::{HeaderMap, StatusCode},
    response::{IntoResponse, Response},
    routing::post,
};
use execution_client::{ExecutionClient, ExecutionClientConfig, GatewayHostRuntime};
use execution_core::*;
use execution_supervisor_core::{
    SupervisorConfig, SupervisorLimits, SupervisorRoot, SupervisorRuntime,
};
use execution_wire::{Operation, RequestEnvelope, ResponseEnvelope, dispatch_request};
use std::sync::{
    Arc,
    atomic::{AtomicBool, AtomicUsize, Ordering},
};
use tool_read::{ReadConfig, ReadInput, ReadTool};
use tool_write::{ObservedFile, WriteConfig, WriteInput, WriteOutput, WriteState, WriteTool};

struct Host {
    temp: tempfile::TempDir,
    runtime: GatewayHostRuntime,
    writes: Arc<AtomicUsize>,
    calls: Arc<AtomicUsize>,
    server: tokio::task::JoinHandle<()>,
}
impl Drop for Host {
    fn drop(&mut self) {
        self.server.abort();
    }
}
impl Host {
    async fn new(mode: &str) -> Self {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().join("workspace");
        std::fs::create_dir_all(root.join("project")).unwrap();
        std::fs::write(root.join("project/existing.txt"), "original\r\n").unwrap();
        let supervisor = Arc::new(
            SupervisorRuntime::new(SupervisorConfig {
                host_id: ExecutionHostId::generate(),
                state_directory: temp.path().join("state"),
                roots: vec![SupervisorRoot {
                    id: RootId::new("work").unwrap(),
                    name: "Work".into(),
                    path: root,
                    read_only: mode == "read-only",
                }],
                limits: SupervisorLimits {
                    max_write_bytes: if mode == "small-limit" {
                        8
                    } else {
                        16 * 1024 * 1024
                    },
                    ..Default::default()
                },
            })
            .await
            .unwrap(),
        );
        let writes = Arc::new(AtomicUsize::new(0));
        let calls = Arc::new(AtomicUsize::new(0));
        let count = writes.clone();
        let all = calls.clone();
        let target = supervisor.clone();
        let lose_reply = Arc::new(AtomicBool::new(mode == "lost-reply"));
        let stalled_reply = mode == "stalled-reply";
        let app = Router::new().route(
            "/v1/hosts/{host}/operations",
            post(
                move |Path(id): Path<String>,
                      headers: HeaderMap,
                      Json(request): Json<RequestEnvelope>| {
                    let target = target.clone();
                    let count = count.clone();
                    let all = all.clone();
                    let lose = lose_reply.clone();
                    async move {
                        assert_eq!(headers["authorization"], "Bearer write-tool-test-token");
                        assert_eq!(id, target.descriptor().host_id.as_str());
                        all.fetch_add(1, Ordering::SeqCst);
                        let write = matches!(request.operation, Operation::FilesystemWrite(_));
                        if write {
                            count.fetch_add(1, Ordering::SeqCst);
                        }
                        let reply =
                            dispatch_request(target.as_ref(), &OperationContext::new(), request)
                                .await;
                        if write && matches!(reply, ResponseEnvelope::Success { .. }) {
                            if lose.swap(false, Ordering::SeqCst) {
                                return (StatusCode::BAD_GATEWAY, "reply lost after mutation")
                                    .into_response();
                            }
                            if stalled_reply {
                                return std::future::pending::<Response>().await;
                            }
                        }
                        Json(reply).into_response()
                    }
                },
            ),
        );
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("http://{}", listener.local_addr().unwrap());
        let server = tokio::spawn(async move {
            axum::serve(listener, app).await.unwrap();
        });
        let mut config = ExecutionClientConfig::new(url.parse().unwrap(), "write-tool-test-token");
        config.allow_insecure_http = true;
        let runtime = ExecutionClient::new(config)
            .unwrap()
            .connect_host(&context(), supervisor.descriptor().host_id.clone())
            .await
            .unwrap();
        Self {
            temp,
            runtime,
            writes,
            calls,
            server,
        }
    }
    fn cwd(&self) -> ExecutionPath {
        ExecutionPath::new(RootId::new("work").unwrap(), "project").unwrap()
    }
    fn tool(&self) -> WriteTool<'_> {
        WriteTool::new(&self.runtime, self.cwd(), WriteConfig::default()).unwrap()
    }
    fn disk_path(&self, path: &str) -> std::path::PathBuf {
        self.temp.path().join("workspace/project").join(path)
    }
    async fn observed(&self, path: &str) -> ObservedFile {
        let read = ReadTool::new(&self.runtime, self.cwd(), ReadConfig::default())
            .unwrap()
            .execute(
                &context(),
                ReadInput {
                    file_path: path.into(),
                    offset: None,
                    limit: None,
                },
            )
            .await
            .unwrap();
        ObservedFile {
            host_id: self.runtime.descriptor().host_id.clone(),
            path: read.path,
            revision: read.revision,
        }
    }
    async fn write(
        &self,
        path: &str,
        content: &str,
        observed: Option<ObservedFile>,
    ) -> ExecutionResult<WriteOutput> {
        self.tool()
            .execute(&context(), input(path, content), state(observed))
            .await
    }
}
fn context() -> OperationContext {
    OperationContext::with_timeout(std::time::Duration::from_secs(5))
}
fn input(path: &str, content: &str) -> WriteInput {
    WriteInput {
        file_path: path.into(),
        content: content.into(),
    }
}
fn state(observed: Option<ObservedFile>) -> WriteState {
    WriteState {
        operation_id: OperationId::generate(),
        observed,
    }
}

#[tokio::test]
async fn creates_parents_preserves_exact_bytes_and_returns_a_readable_revision() {
    let host = Host::new("").await;
    let content = "\tπ🙂\r\nlast";
    let result = host.write("new/nested.txt", content, None).await.unwrap();
    assert!(result.created);
    assert_eq!(result.bytes_written, content.len() as u64);
    assert_eq!(
        std::fs::read(host.disk_path("new/nested.txt")).unwrap(),
        content.as_bytes()
    );
    assert_eq!(
        host.observed("new/nested.txt").await.revision,
        result.revision
    );
    assert!(result.to_text().contains("new/nested.txt"));
    assert!(!result.to_text().contains(content));
    assert_eq!(serde_json::to_value(&result).unwrap()["created"], true);
    let replaced = host
        .write("new/nested.txt", "next", Some(result.observation()))
        .await
        .unwrap();
    assert!(!replaced.created);
    assert_eq!(
        std::fs::read(host.disk_path("new/nested.txt")).unwrap(),
        b"next"
    );
}

#[tokio::test]
async fn existing_files_require_observation_and_reject_stale_or_deleted_versions() {
    let host = Host::new("").await;
    let error = host
        .write("existing.txt", "overwrite", None)
        .await
        .unwrap_err();
    assert_eq!(error.code, ExecutionErrorCode::AlreadyExists);
    assert!(error.message.contains("read it"));
    assert_eq!(
        std::fs::read(host.disk_path("existing.txt")).unwrap(),
        b"original\r\n"
    );
    let observed = host.observed("existing.txt").await;
    std::fs::write(host.disk_path("existing.txt"), "external change").unwrap();
    let error = host
        .write("existing.txt", "overwrite", Some(observed.clone()))
        .await
        .unwrap_err();
    assert_eq!(error.code, ExecutionErrorCode::RevisionConflict);
    assert!(error.message.contains("read it again"));
    assert_eq!(
        std::fs::read(host.disk_path("existing.txt")).unwrap(),
        b"external change"
    );
    std::fs::remove_file(host.disk_path("existing.txt")).unwrap();
    assert_eq!(
        host.write("existing.txt", "recreate", Some(observed))
            .await
            .unwrap_err()
            .code,
        ExecutionErrorCode::RevisionConflict
    );
    assert!(!host.disk_path("existing.txt").exists());
}

#[tokio::test]
async fn empty_strings_create_and_truncate_and_nul_is_written_exactly() {
    let host = Host::new("").await;
    let new = host.write("empty.txt", "", None).await.unwrap();
    assert!(new.created);
    assert_eq!(new.bytes_written, 0);
    let old = host
        .write(
            "existing.txt",
            "",
            Some(host.observed("existing.txt").await),
        )
        .await
        .unwrap();
    assert!(!old.created);
    assert_eq!(old.bytes_written, 0);
    assert_eq!(std::fs::read(host.disk_path("existing.txt")).unwrap(), b"");
    host.write("nul.txt", "a\0b", None).await.unwrap();
    assert_eq!(std::fs::read(host.disk_path("nul.txt")).unwrap(), b"a\0b");
}

#[tokio::test]
async fn observations_cannot_be_reused_for_other_paths_or_hosts() {
    let host = Host::new("").await;
    let observed = host.observed("existing.txt").await;
    let before = host.calls.load(Ordering::SeqCst);
    let mut wrong_host = observed.clone();
    wrong_host.host_id = ExecutionHostId::generate();
    let mut wrong_root = observed.clone();
    wrong_root.path.root_id = RootId::new("other").unwrap();
    for (path, observed) in [
        ("another.txt", observed),
        ("existing.txt", wrong_host),
        ("existing.txt", wrong_root),
    ] {
        assert_eq!(
            host.write(path, "wrong", Some(observed))
                .await
                .unwrap_err()
                .code,
            ExecutionErrorCode::InvalidRequest
        );
    }
    assert_eq!(host.calls.load(Ordering::SeqCst), before);
}

#[tokio::test]
async fn concurrent_revision_checked_writes_allow_only_one_winner() {
    let host = Host::new("").await;
    let observed = host.observed("existing.txt").await;
    let (a, b) = tokio::join!(
        host.write("existing.txt", "a", Some(observed.clone())),
        host.write("existing.txt", "b", Some(observed))
    );
    assert_ne!(a.is_ok(), b.is_ok());
    let loser = if let Err(error) = a {
        error
    } else {
        b.unwrap_err()
    };
    assert_eq!(loser.code, ExecutionErrorCode::RevisionConflict);
    let (a, b) = tokio::join!(
        host.write("new.txt", "a", None),
        host.write("new.txt", "b", None)
    );
    assert_ne!(a.is_ok(), b.is_ok());
    let loser = if let Err(error) = a {
        error
    } else {
        b.unwrap_err()
    };
    assert_eq!(loser.code, ExecutionErrorCode::AlreadyExists);
}

#[tokio::test]
async fn lost_responses_are_not_retried_and_explicit_replays_return_original_receipts() {
    let host = Host::new("lost-reply").await;
    let tool = host.tool();
    let request = input("new.txt", "written once");
    let state = state(None);
    let serialized = serde_json::to_value((&request, &state)).unwrap();
    assert!(
        tool.execute(&context(), request.clone(), state.clone())
            .await
            .is_err()
    );
    assert_eq!(host.writes.load(Ordering::SeqCst), 1);
    assert_eq!(
        std::fs::read(host.disk_path("new.txt")).unwrap(),
        b"written once"
    );
    std::fs::write(host.disk_path("new.txt"), "later external content").unwrap();
    let (restored_input, restored_state): (WriteInput, WriteState) =
        serde_json::from_value(serialized).unwrap();
    let result = tool
        .execute(&context(), restored_input, restored_state)
        .await
        .unwrap();
    assert!(result.created);
    assert_eq!(result.bytes_written, 12);
    assert_eq!(
        std::fs::read(host.disk_path("new.txt")).unwrap(),
        b"later external content"
    );
    let error = tool
        .execute(&context(), input("new.txt", "different payload"), state)
        .await
        .unwrap_err();
    assert_eq!(error.code, ExecutionErrorCode::OperationConflict);
}

#[tokio::test]
async fn overwrite_replay_retains_the_original_observation() {
    let host = Host::new("").await;
    let observed = host.observed("existing.txt").await;
    let state = state(Some(observed));
    let tool = host.tool();
    let request = input("existing.txt", "updated");
    let result = tool
        .execute(&context(), request.clone(), state.clone())
        .await
        .unwrap();
    let replay = tool
        .execute(&context(), request.clone(), state.clone())
        .await
        .unwrap();
    assert_eq!(result.revision, replay.revision);
    let changed_state = WriteState {
        operation_id: state.operation_id,
        observed: Some(result.observation()),
    };
    assert_eq!(
        tool.execute(&context(), request, changed_state)
            .await
            .unwrap_err()
            .code,
        ExecutionErrorCode::OperationConflict
    );
}

#[tokio::test]
async fn path_and_limit_validation_precede_mutation() {
    let host = Host::new("small-limit").await;
    let before = host.calls.load(Ordering::SeqCst);
    assert_eq!(
        host.write("new.txt", "🙂🙂🙂", None)
            .await
            .unwrap_err()
            .code,
        ExecutionErrorCode::ResourceExhausted
    );
    let tool = WriteTool::new(
        &host.runtime,
        host.cwd(),
        WriteConfig {
            max_content_bytes: 2,
        },
    )
    .unwrap();
    assert_eq!(
        tool.execute(&context(), input("new.txt", "abc"), state(None))
            .await
            .unwrap_err()
            .code,
        ExecutionErrorCode::ResourceExhausted
    );
    for path in ["../escape", "/outside/file", "", "/"] {
        assert!(host.write(path, "a", None).await.is_err());
    }
    assert_eq!(host.calls.load(Ordering::SeqCst), before);
    let path = format!(
        "{}/project/new.txt",
        host.runtime.descriptor().roots[0].native_path
    );
    host.write(&path, "absolute", None).await.unwrap();
    assert_eq!(
        std::fs::read(host.disk_path("new.txt")).unwrap(),
        b"absolute"
    );
    let readonly = Host::new("read-only").await;
    assert_eq!(
        readonly.write("new.txt", "a", None).await.unwrap_err().code,
        ExecutionErrorCode::ReadOnlyRoot
    );
    assert_eq!(readonly.writes.load(Ordering::SeqCst), 0);
}

#[tokio::test]
async fn cancellation_can_follow_a_completed_remote_write() {
    let host = Host::new("").await;
    let cancelled = context();
    cancelled.cancel();
    assert_eq!(
        host.tool()
            .execute(&cancelled, input("new.txt", "a"), state(None))
            .await
            .unwrap_err()
            .code,
        ExecutionErrorCode::Cancelled
    );
    assert_eq!(host.writes.load(Ordering::SeqCst), 0);
    let host = Host::new("stalled-reply").await;
    let ctx = OperationContext::with_timeout(std::time::Duration::from_millis(150));
    let error = tokio::time::timeout(
        std::time::Duration::from_secs(3),
        host.tool()
            .execute(&ctx, input("new.txt", "done"), state(None)),
    )
    .await
    .unwrap()
    .unwrap_err();
    assert_eq!(error.code, ExecutionErrorCode::DeadlineExceeded);
    assert_eq!(host.writes.load(Ordering::SeqCst), 1);
    assert_eq!(std::fs::read(host.disk_path("new.txt")).unwrap(), b"done");
}

#[cfg(unix)]
#[tokio::test]
async fn symlinks_follow_contained_targets_and_reject_escape() {
    let host = Host::new("").await;
    std::os::unix::fs::symlink("existing.txt", host.disk_path("link.txt")).unwrap();
    let observed = host.observed("link.txt").await;
    host.write("link.txt", "through link", Some(observed))
        .await
        .unwrap();
    assert_eq!(
        std::fs::read(host.disk_path("existing.txt")).unwrap(),
        b"through link"
    );
    assert!(
        std::fs::symlink_metadata(host.disk_path("link.txt"))
            .unwrap()
            .is_symlink()
    );
    let outside = host.temp.path().join("outside.txt");
    std::fs::write(&outside, "outside").unwrap();
    std::os::unix::fs::symlink(&outside, host.disk_path("escape.txt")).unwrap();
    assert_eq!(
        host.write("escape.txt", "wrong", None)
            .await
            .unwrap_err()
            .code,
        ExecutionErrorCode::PathOutsideRoot
    );
    assert_eq!(std::fs::read(outside).unwrap(), b"outside");
}

#[test]
fn model_schema_has_only_the_two_agreed_fields() {
    let schema = jsonschema::validator_for(&tool_write::input_schema()).unwrap();
    for content in ["", "hello\r\n", "\0"] {
        assert!(schema.is_valid(&serde_json::json!({"file_path":"file", "content":content})));
    }
    for value in [
        serde_json::json!({"file_path":"file"}),
        serde_json::json!({"file_path":"", "content":"x"}),
        serde_json::json!({"file_path":"file", "content":3}),
        serde_json::json!({"file_path":"file", "content":"x", "revision":"bypass"}),
    ] {
        assert!(!schema.is_valid(&value));
    }
    assert!(
        serde_json::from_value::<WriteInput>(
            serde_json::json!({"file_path":"file", "content":"x", "operation_id":"bypass"})
        )
        .is_err()
    );
}
