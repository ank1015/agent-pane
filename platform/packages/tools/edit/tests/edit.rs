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
use execution_wire::{
    Operation, OperationResult, RequestEnvelope, ResponseEnvelope, dispatch_request,
};
use std::sync::{
    Arc,
    atomic::{AtomicBool, AtomicUsize, Ordering},
};
use tool_edit::{EditConfig, EditInput, EditState, EditTool, ObservedFile, PreparedEdit};
use tool_read::{ReadConfig, ReadInput, ReadTool};

struct Host {
    temp: tempfile::TempDir,
    runtime: GatewayHostRuntime,
    calls: Arc<AtomicUsize>,
    writes: Arc<AtomicUsize>,
    task: tokio::task::JoinHandle<()>,
}
impl Drop for Host {
    fn drop(&mut self) {
        self.task.abort();
    }
}
impl Host {
    async fn new(content: &[u8], mode: &str) -> Self {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().join("workspace");
        std::fs::create_dir_all(root.join("project")).unwrap();
        std::fs::write(root.join("project/file.txt"), content).unwrap();
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
                    max_read_bytes: if mode == "small-read" {
                        2
                    } else {
                        16 * 1024 * 1024
                    },
                    max_write_bytes: if mode == "small-write" {
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
        let calls = Arc::new(AtomicUsize::new(0));
        let writes = Arc::new(AtomicUsize::new(0));
        let call_count = calls.clone();
        let write_count = writes.clone();
        let target = supervisor.clone();
        let lost = Arc::new(AtomicBool::new(mode == "lost"));
        let mode = mode.to_owned();
        let app = Router::new().route(
            "/v1/hosts/{host}/operations",
            post(
                move |Path(id): Path<String>,
                      headers: HeaderMap,
                      Json(request): Json<RequestEnvelope>| {
                    let target = target.clone();
                    let calls = call_count.clone();
                    let writes = write_count.clone();
                    let lost = lost.clone();
                    let mode = mode.clone();
                    async move {
                        assert_eq!(headers["authorization"], "Bearer edit-tool-test-token");
                        assert_eq!(id, target.descriptor().host_id.as_str());
                        calls.fetch_add(1, Ordering::SeqCst);
                        let write = matches!(request.operation, Operation::FilesystemWrite(_));
                        let read = matches!(request.operation, Operation::FilesystemRead(_));
                        if write {
                            writes.fetch_add(1, Ordering::SeqCst);
                        }
                        if read && mode == "stall-read" {
                            return std::future::pending::<Response>().await;
                        }
                        let mut reply =
                            dispatch_request(target.as_ref(), &OperationContext::new(), request)
                                .await;
                        if let ResponseEnvelope::Success {
                            result: OperationResult::ReadFile(value),
                            ..
                        } = &mut reply
                        {
                            if mode == "revision" && value.offset > 0 {
                                value.metadata.revision =
                                    Some(FileRevision::new("changed").unwrap());
                            }
                            if mode == "offset" {
                                value.offset += 1;
                            }
                            if mode == "path" {
                                value.metadata.path.path = "wrong".into();
                            }
                            if mode == "empty" {
                                value.data = BinaryData::default();
                                value.eof = false;
                            }
                            if mode == "eof" {
                                value.eof = true;
                            }
                        }
                        if write && matches!(reply, ResponseEnvelope::Success { .. }) {
                            if lost.swap(false, Ordering::SeqCst) {
                                return (StatusCode::BAD_GATEWAY, "lost reply").into_response();
                            }
                            if mode == "stall-write" {
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
        let task = tokio::spawn(async move {
            axum::serve(listener, app).await.unwrap();
        });
        let mut config = ExecutionClientConfig::new(url.parse().unwrap(), "edit-tool-test-token");
        config.allow_insecure_http = true;
        let runtime = ExecutionClient::new(config)
            .unwrap()
            .connect_host(&context(), supervisor.descriptor().host_id.clone())
            .await
            .unwrap();
        Self {
            temp,
            runtime,
            calls,
            writes,
            task,
        }
    }
    fn cwd(&self) -> ExecutionPath {
        ExecutionPath::new(RootId::new("work").unwrap(), "project").unwrap()
    }
    fn path(&self) -> ExecutionPath {
        ExecutionPath::new(RootId::new("work").unwrap(), "project/file.txt").unwrap()
    }
    fn disk(&self) -> std::path::PathBuf {
        self.temp.path().join("workspace/project/file.txt")
    }
    fn tool(&self) -> EditTool<'_> {
        EditTool::new(&self.runtime, self.cwd(), EditConfig::default()).unwrap()
    }
    // Metadata-only fixture observations let malformed/binary cases reach edit.
    async fn state(&self) -> EditState {
        let metadata = self
            .runtime
            .filesystem()
            .stat(
                &context(),
                StatRequest {
                    path: self.path(),
                    follow_symlinks: true,
                },
            )
            .await
            .unwrap();
        EditState {
            operation_id: OperationId::generate(),
            observed: ObservedFile {
                host_id: self.runtime.descriptor().host_id.clone(),
                path: metadata.path,
                revision: metadata.revision.unwrap(),
            },
        }
    }
}
fn context() -> OperationContext {
    OperationContext::with_timeout(std::time::Duration::from_secs(5))
}
fn input(old: &str, new: &str, all: bool) -> EditInput {
    EditInput {
        file_path: "file.txt".into(),
        old_string: old.into(),
        new_string: new.into(),
        replace_all: all,
    }
}

#[tokio::test]
async fn read_observation_edit_and_next_observation_work_together() {
    let original = "\u{feff}fn main() {\r\n\tlet x = \"π🙂\";\r\n}";
    let host = Host::new(original.as_bytes(), "small-read").await;
    let read = ReadTool::new(&host.runtime, host.cwd(), ReadConfig::default())
        .unwrap()
        .execute(
            &context(),
            ReadInput {
                file_path: "file.txt".into(),
                offset: None,
                limit: None,
            },
        )
        .await
        .unwrap();
    let state = EditState {
        operation_id: OperationId::generate(),
        observed: ObservedFile {
            host_id: host.runtime.descriptor().host_id.clone(),
            path: read.path,
            revision: read.revision,
        },
    };
    let tool = host.tool();
    let prepared = tool
        .prepare(&context(), input("π🙂", "$1\\literal🙂", false), state)
        .await
        .unwrap();
    assert_eq!(host.writes.load(Ordering::SeqCst), 0);
    assert_eq!(std::fs::read(host.disk()).unwrap(), original.as_bytes());
    assert_eq!(prepared.replacements(), 1);
    assert!(!format!("{prepared:?}").contains("literal"));
    let result = tool.apply(&context(), &prepared).await.unwrap();
    assert_eq!(
        std::fs::read(host.disk()).unwrap(),
        original.replace("π🙂", "$1\\literal🙂").as_bytes()
    );
    assert_eq!(result.bytes_written, prepared.content().len() as u64);
    assert_eq!(result.replacements, 1);
    assert!(result.to_text().starts_with("Replaced 1 occurrence"));
    assert_eq!(serde_json::to_value(&result).unwrap()["replacements"], 1);
    let next = tool
        .prepare(
            &context(),
            input("$1\\literal🙂", "done", false),
            EditState {
                operation_id: OperationId::generate(),
                observed: result.observation(),
            },
        )
        .await
        .unwrap();
    tool.apply(&context(), &next).await.unwrap();
    assert_eq!(
        std::fs::read_to_string(host.disk()).unwrap(),
        original.replace("π🙂", "done")
    );
}

#[tokio::test]
async fn exact_matching_rejects_missing_whitespace_line_ending_and_ambiguous_searches() {
    let host = Host::new(b"  foo\r\nfoo\r\n", "").await;
    let tool = host.tool();
    let state = host.state().await;
    for old in ["foo\n", "missing", " foo\r\nfoo\n", "foo"] {
        assert_eq!(
            tool.prepare(&context(), input(old, "new", false), state.clone())
                .await
                .unwrap_err()
                .code,
            ExecutionErrorCode::InvalidRequest
        );
    }
    assert_eq!(host.writes.load(Ordering::SeqCst), 0);
    let all = tool
        .prepare(&context(), input("foo", "", true), state)
        .await
        .unwrap();
    assert_eq!(all.content(), "  \r\n\r\n");
    assert_eq!(all.replacements(), 2);
    tool.apply(&context(), &all).await.unwrap();
    assert_eq!(std::fs::read(host.disk()).unwrap(), b"  \r\n\r\n");
}

#[tokio::test]
async fn overlapping_matches_are_ambiguous_unless_replace_all_is_requested() {
    for (source, old, expected) in [("aaa", "aa", "Xa"), ("🙂🙂🙂", "🙂🙂", "X🙂")] {
        let host = Host::new(source.as_bytes(), "").await;
        let state = host.state().await;
        assert!(
            host.tool()
                .prepare(&context(), input(old, "X", false), state.clone())
                .await
                .is_err()
        );
        let plan = host
            .tool()
            .prepare(&context(), input(old, "X", true), state)
            .await
            .unwrap();
        assert_eq!(plan.content(), expected);
        assert_eq!(plan.replacements(), 1);
    }
}

#[tokio::test]
async fn stale_before_prepare_or_between_prepare_and_apply_never_overwrites() {
    let host = Host::new(b"old", "").await;
    let state = host.state().await;
    let plan = host
        .tool()
        .prepare(&context(), input("old", "new", false), state.clone())
        .await
        .unwrap();
    std::fs::write(host.disk(), "external").unwrap();
    assert_eq!(
        host.tool()
            .prepare(&context(), input("old", "new", false), state)
            .await
            .unwrap_err()
            .code,
        ExecutionErrorCode::RevisionConflict
    );
    assert_eq!(
        host.tool().apply(&context(), &plan).await.unwrap_err().code,
        ExecutionErrorCode::RevisionConflict
    );
    assert_eq!(std::fs::read(host.disk()).unwrap(), b"external");
    std::fs::remove_file(host.disk()).unwrap();
    assert_eq!(
        host.tool().apply(&context(), &plan).await.unwrap_err().code,
        ExecutionErrorCode::RevisionConflict
    );
    assert!(!host.disk().exists());
}

#[tokio::test]
async fn persisted_plan_recovers_a_lost_reply_without_recomputing_or_reapplying() {
    let host = Host::new(b"old", "lost").await;
    let plan = host
        .tool()
        .prepare(&context(), input("old", "new", false), host.state().await)
        .await
        .unwrap();
    let saved = serde_json::to_vec(&plan).unwrap();
    assert!(host.tool().apply(&context(), &plan).await.is_err());
    assert_eq!(host.writes.load(Ordering::SeqCst), 1);
    assert_eq!(std::fs::read(host.disk()).unwrap(), b"new");
    std::fs::write(host.disk(), "later edit").unwrap();
    let restored: PreparedEdit = serde_json::from_slice(&saved).unwrap();
    let tool = EditTool::new(
        &host.runtime,
        ExecutionPath::root(RootId::new("work").unwrap()),
        EditConfig::default(),
    )
    .unwrap();
    let before = host.calls.load(Ordering::SeqCst);
    let result = tool.apply(&context(), &restored).await.unwrap();
    assert_eq!(host.calls.load(Ordering::SeqCst), before + 1); // Only the original write is replayed.
    assert_eq!(result.bytes_written, 3);
    assert_eq!(std::fs::read(host.disk()).unwrap(), b"later edit");
    let mut changed = serde_json::to_value(&restored).unwrap();
    changed["content"] = "changed payload".into();
    let changed: PreparedEdit = serde_json::from_value(changed).unwrap();
    assert_eq!(
        tool.apply(&context(), &changed).await.unwrap_err().code,
        ExecutionErrorCode::OperationConflict
    );
}

#[tokio::test]
async fn concurrent_prepared_edits_have_one_winner() {
    let host = Host::new(b"old", "").await;
    let tool = host.tool();
    let a = tool
        .prepare(&context(), input("old", "a", false), host.state().await)
        .await
        .unwrap();
    let b = tool
        .prepare(&context(), input("old", "b", false), host.state().await)
        .await
        .unwrap();
    let context = context();
    let (a, b) = tokio::join!(tool.apply(&context, &a), tool.apply(&context, &b));
    assert_ne!(a.is_ok(), b.is_ok());
    let error = match a {
        Err(error) => error,
        Ok(_) => b.unwrap_err(),
    };
    assert_eq!(error.code, ExecutionErrorCode::RevisionConflict);
}

#[tokio::test]
async fn invalid_inputs_and_wrong_observations_fail_without_network_io() {
    let host = Host::new(b"old", "").await;
    let state = host.state().await;
    let before = host.calls.load(Ordering::SeqCst);
    for request in [
        input("", "new", false),
        input("old", "old", false),
        EditInput {
            file_path: "../escape".into(),
            ..input("old", "new", false)
        },
    ] {
        assert!(
            host.tool()
                .prepare(&context(), request, state.clone())
                .await
                .is_err()
        );
    }
    let mut wrong_host = state.clone();
    wrong_host.observed.host_id = ExecutionHostId::generate();
    let mut wrong_path = state.clone();
    wrong_path.observed.path.path = "project/other".into();
    for state in [wrong_host, wrong_path] {
        assert!(
            host.tool()
                .prepare(&context(), input("old", "new", false), state)
                .await
                .is_err()
        );
    }
    assert_eq!(host.calls.load(Ordering::SeqCst), before);
    let plan = host
        .tool()
        .prepare(&context(), input("old", "new", false), state)
        .await
        .unwrap();
    let other = Host::new(b"old", "").await;
    assert_eq!(
        other
            .tool()
            .apply(&context(), &plan)
            .await
            .unwrap_err()
            .code,
        ExecutionErrorCode::InvalidRequest
    );
    assert_eq!(other.writes.load(Ordering::SeqCst), 0);
}

#[tokio::test]
async fn full_file_reads_reject_malformed_data_and_revision_changes() {
    for (mode, code) in [
        ("revision", ExecutionErrorCode::RevisionConflict),
        ("offset", ExecutionErrorCode::Internal),
        ("path", ExecutionErrorCode::Internal),
        ("empty", ExecutionErrorCode::Internal),
        ("eof", ExecutionErrorCode::Internal),
    ] {
        let host = Host::new(b"old content", mode).await;
        let tool = EditTool::new(
            &host.runtime,
            host.cwd(),
            EditConfig {
                chunk_bytes: 2,
                ..Default::default()
            },
        )
        .unwrap();
        assert_eq!(
            tool.prepare(&context(), input("old", "new", false), host.state().await)
                .await
                .unwrap_err()
                .code,
            code,
            "{mode}"
        );
        assert_eq!(host.writes.load(Ordering::SeqCst), 0);
    }
    for bytes in [b"old\0binary".as_slice(), &[0xff, b'o', b'l', b'd']] {
        let host = Host::new(bytes, "").await;
        assert_eq!(
            host.tool()
                .prepare(&context(), input("old", "new", false), host.state().await)
                .await
                .unwrap_err()
                .code,
            ExecutionErrorCode::Unsupported
        );
    }
}

#[tokio::test]
async fn file_and_expansion_limits_fail_before_mutation() {
    let host = Host::new(b"aaaaaaaa", "small-write").await;
    let state = host.state().await;
    assert_eq!(
        host.tool()
            .prepare(&context(), input("a", "xx", true), state.clone())
            .await
            .unwrap_err()
            .code,
        ExecutionErrorCode::ResourceExhausted
    );
    let limited = EditTool::new(
        &host.runtime,
        host.cwd(),
        EditConfig {
            max_file_bytes: 4,
            ..Default::default()
        },
    )
    .unwrap();
    assert_eq!(
        limited
            .prepare(&context(), input("a", "b", true), state.clone())
            .await
            .unwrap_err()
            .code,
        ExecutionErrorCode::ResourceExhausted
    );
    assert_eq!(host.writes.load(Ordering::SeqCst), 0);
    let deletion = host
        .tool()
        .prepare(&context(), input("a", "", true), state)
        .await
        .unwrap();
    assert_eq!(deletion.replacements(), 8);
    host.tool().apply(&context(), &deletion).await.unwrap();
    assert_eq!(std::fs::read(host.disk()).unwrap(), b"");
    assert_eq!(
        host.tool()
            .prepare(&context(), input("a", "b", false), host.state().await)
            .await
            .unwrap_err()
            .code,
        ExecutionErrorCode::InvalidRequest
    );
}

#[tokio::test]
async fn missing_directory_readonly_and_absolute_paths_follow_filesystem_rules() {
    let host = Host::new(b"old", "").await;
    let state = host.state().await;
    let mut request = input("old", "new", false);
    request.file_path = format!(
        "{}/project/file.txt",
        host.runtime.descriptor().roots[0].native_path
    );
    let plan = host
        .tool()
        .prepare(&context(), request, state.clone())
        .await
        .unwrap();
    assert_eq!(plan.state().observed.path, host.path());
    std::fs::remove_file(host.disk()).unwrap();
    assert_eq!(
        host.tool()
            .prepare(&context(), input("old", "new", false), state.clone())
            .await
            .unwrap_err()
            .code,
        ExecutionErrorCode::NotFound
    );
    std::fs::create_dir(host.disk()).unwrap();
    assert_eq!(
        host.tool()
            .prepare(&context(), input("old", "new", false), state)
            .await
            .unwrap_err()
            .code,
        ExecutionErrorCode::IsDirectory
    );
    let readonly = Host::new(b"old", "read-only").await;
    let state = readonly.state().await;
    let before = readonly.calls.load(Ordering::SeqCst);
    assert_eq!(
        readonly
            .tool()
            .prepare(&context(), input("old", "new", false), state)
            .await
            .unwrap_err()
            .code,
        ExecutionErrorCode::ReadOnlyRoot
    );
    assert_eq!(readonly.calls.load(Ordering::SeqCst), before);
}

#[tokio::test]
async fn cancellation_bounds_preparation_and_can_follow_a_completed_write() {
    let host = Host::new(b"old", "stall-read").await;
    let state = host.state().await;
    let cancelled = context();
    cancelled.cancel();
    let before = host.calls.load(Ordering::SeqCst);
    assert_eq!(
        host.tool()
            .prepare(&cancelled, input("old", "new", false), state.clone())
            .await
            .unwrap_err()
            .code,
        ExecutionErrorCode::Cancelled
    );
    assert_eq!(host.calls.load(Ordering::SeqCst), before);
    let deadline = OperationContext::with_timeout(std::time::Duration::from_millis(150));
    assert_eq!(
        host.tool()
            .prepare(&deadline, input("old", "new", false), state)
            .await
            .unwrap_err()
            .code,
        ExecutionErrorCode::DeadlineExceeded
    );
    let host = Host::new(b"old", "stall-write").await;
    let plan = host
        .tool()
        .prepare(&context(), input("old", "new", false), host.state().await)
        .await
        .unwrap();
    let cancelled = context();
    cancelled.cancel();
    assert_eq!(
        host.tool().apply(&cancelled, &plan).await.unwrap_err().code,
        ExecutionErrorCode::Cancelled
    );
    assert_eq!(host.writes.load(Ordering::SeqCst), 0);
    let deadline = OperationContext::with_timeout(std::time::Duration::from_millis(150));
    assert_eq!(
        host.tool().apply(&deadline, &plan).await.unwrap_err().code,
        ExecutionErrorCode::DeadlineExceeded
    );
    assert_eq!(std::fs::read(host.disk()).unwrap(), b"new");
    assert_eq!(host.writes.load(Ordering::SeqCst), 1);
}

#[test]
fn schema_and_deserialization_enforce_the_model_contract() {
    let schema = jsonschema::validator_for(&tool_edit::input_schema()).unwrap();
    let minimal = serde_json::json!({"file_path":"file", "old_string":"old", "new_string":""});
    assert!(schema.is_valid(&minimal));
    assert!(
        !serde_json::from_value::<EditInput>(minimal)
            .unwrap()
            .replace_all
    );
    for value in [
        serde_json::json!({"file_path":"file", "old_string":"", "new_string":"new"}),
        serde_json::json!({"file_path":"file", "old_string":"old"}),
        serde_json::json!({"file_path":"file", "old_string":"old", "new_string":"new", "replace_all":"true"}),
        serde_json::json!({"file_path":"file", "old_string":"old", "new_string":"new", "revision":"bypass"}),
    ] {
        assert!(!schema.is_valid(&value));
    }
    assert!(serde_json::from_value::<EditInput>(serde_json::json!({"file_path":"file", "old_string":"a", "new_string":"b", "revision":"bypass"})).is_err());
}
