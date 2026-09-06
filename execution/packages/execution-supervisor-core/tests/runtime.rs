use std::time::Duration;

#[cfg(unix)]
use std::path::PathBuf;

use execution_core::{
    BinaryData, CommandSpec, EnvironmentVariables, ExecutionErrorCode, ExecutionHostId,
    ExecutionId, ExecutionPath, ExecutionRuntime, ExecutionState, FileKind, ListDirectoryRequest,
    OperationContext, OperationId, ProcessEvent, ProcessEventKind, ProcessInput,
    ProcessInputStatus, ReadExecutionRequest, ReadFileRequest, RootId, StartExecutionRequest,
    StdinMode, TerminateExecutionRequest, WriteCondition, WriteFileRequest, WriteId,
    WriteProcessInputRequest,
};
#[cfg(unix)]
use execution_core::{ResizePtyRequest, StatRequest};
use execution_supervisor_core::{
    SupervisorConfig, SupervisorLimits, SupervisorRoot, SupervisorRuntime,
};
use tempfile::TempDir;

struct Fixture {
    _temporary: TempDir,
    #[cfg(unix)]
    root_directory: PathBuf,
    root_id: RootId,
    runtime: SupervisorRuntime,
}

impl Fixture {
    async fn new() -> Self {
        Self::with_limits(SupervisorLimits::default()).await
    }

    async fn with_limits(limits: SupervisorLimits) -> Self {
        let temporary = tempfile::tempdir().expect("create fixture directory");
        let root_directory = temporary.path().join("workspace");
        tokio::fs::create_dir(&root_directory)
            .await
            .expect("create execution root");
        let root_id = RootId::new("workspace").expect("valid root ID");
        let runtime = SupervisorRuntime::new(SupervisorConfig {
            host_id: ExecutionHostId::generate(),
            state_directory: temporary.path().join("state"),
            roots: vec![SupervisorRoot {
                id: root_id.clone(),
                name: "Workspace".to_string(),
                path: root_directory.clone(),
                read_only: false,
            }],
            limits,
        })
        .await
        .expect("create supervisor runtime");
        Self {
            _temporary: temporary,
            #[cfg(unix)]
            root_directory,
            root_id,
            runtime,
        }
    }

    fn path(&self, path: &str) -> ExecutionPath {
        ExecutionPath::new(self.root_id.clone(), path).expect("valid execution path")
    }

    fn start_request(
        &self,
        unix_command: &str,
        windows_command: &str,
        stdin: StdinMode,
    ) -> StartExecutionRequest {
        StartExecutionRequest {
            expected_generation: None,
            output_drain_timeout_ms: None,
            operation_id: OperationId::generate(),
            execution_id: ExecutionId::generate(),
            command: CommandSpec::Shell {
                command: if cfg!(windows) {
                    windows_command.to_string()
                } else {
                    unix_command.to_string()
                },
                shell: if cfg!(windows) {
                    None
                } else {
                    Some("/bin/sh".to_string())
                },
                login: false,
            },
            cwd: ExecutionPath::root(self.root_id.clone()),
            environment: EnvironmentVariables::default(),
            stdin,
            timeout_ms: None,
        }
    }
}

#[tokio::test]
async fn runtime_publishes_generation_descriptor_and_private_state() {
    let fixture = Fixture::new().await;
    let descriptor = fixture.runtime.descriptor();

    assert_eq!(descriptor.roots.len(), 1);
    assert!(descriptor.features.pty);
    assert!(descriptor.features.file_revisions);
    assert_eq!(descriptor.limits.max_process_input_bytes, Some(1024 * 1024));
    assert!(
        fixture
            .runtime
            .generation_state_directory()
            .join("descriptor.json")
            .is_file()
    );
}

#[tokio::test]
async fn filesystem_writes_reads_lists_and_replays_mutations() {
    let fixture = Fixture::new().await;
    let context = OperationContext::new();
    let operation_id = OperationId::generate();
    let request = WriteFileRequest {
        expected_generation: None,
        strategy: execution_core::WriteStrategy::AtomicReplace,
        operation_id: operation_id.clone(),
        path: fixture.path("nested/value.txt"),
        data: BinaryData::new(b"hello world".to_vec()),
        condition: WriteCondition::MustNotExist,
        create_parents: true,
        follow_symlinks: true,
    };

    let first = fixture
        .runtime
        .filesystem()
        .write(&context, request.clone())
        .await
        .expect("write file");
    let replay = fixture
        .runtime
        .filesystem()
        .write(&context, request)
        .await
        .expect("replay write");
    assert_eq!(first, replay);

    let read = fixture
        .runtime
        .filesystem()
        .read(
            &context,
            ReadFileRequest {
                path: fixture.path("nested/value.txt"),
                offset: 6,
                max_bytes: 5,
                follow_symlinks: true,
            },
        )
        .await
        .expect("read range");
    assert_eq!(read.data.as_slice(), b"world");
    assert!(read.eof);
    assert_eq!(read.metadata.revision, Some(first.revision.clone()));

    let listing = fixture
        .runtime
        .filesystem()
        .list(
            &context,
            ListDirectoryRequest {
                path: fixture.path("nested"),
                follow_symlinks: false,
                max_entries: 10,
                cursor: None,
            },
        )
        .await
        .expect("list directory");
    assert_eq!(listing.entries.len(), 1);
    assert_eq!(listing.entries[0].name, "value.txt");
    assert_eq!(listing.entries[0].metadata.kind, FileKind::File);

    let conflict = fixture
        .runtime
        .filesystem()
        .write(
            &context,
            WriteFileRequest {
                expected_generation: None,
                strategy: execution_core::WriteStrategy::AtomicReplace,
                operation_id,
                path: fixture.path("nested/other.txt"),
                data: BinaryData::new(b"different".to_vec()),
                condition: WriteCondition::Any,
                create_parents: true,
                follow_symlinks: true,
            },
        )
        .await
        .expect_err("reused operation ID must conflict");
    assert_eq!(conflict.code, ExecutionErrorCode::OperationConflict);

    let stale = fixture
        .runtime
        .filesystem()
        .write(
            &context,
            WriteFileRequest {
                expected_generation: None,
                strategy: execution_core::WriteStrategy::AtomicReplace,
                operation_id: OperationId::generate(),
                path: fixture.path("nested/value.txt"),
                data: BinaryData::new(b"replacement".to_vec()),
                condition: WriteCondition::MatchRevision {
                    revision: execution_core::FileRevision::new("stale").expect("valid revision"),
                },
                create_parents: true,
                follow_symlinks: true,
            },
        )
        .await
        .expect_err("stale revision must fail");
    assert_eq!(stale.code, ExecutionErrorCode::RevisionConflict);
}

#[cfg(unix)]
#[tokio::test]
async fn filesystem_confines_symlinks_and_can_replace_a_final_link() {
    use std::os::unix::fs::symlink;

    let fixture = Fixture::new().await;
    let outside = fixture
        .root_directory
        .parent()
        .expect("workspace has parent")
        .join("outside.txt");
    tokio::fs::write(&outside, b"secret")
        .await
        .expect("write outside file");
    let link = fixture.root_directory.join("link.txt");
    symlink(&outside, &link).expect("create symlink");

    let escaped = fixture
        .runtime
        .filesystem()
        .stat(
            &OperationContext::new(),
            StatRequest {
                path: fixture.path("link.txt"),
                follow_symlinks: true,
            },
        )
        .await
        .expect_err("followed symlink must stay inside root");
    assert_eq!(escaped.code, ExecutionErrorCode::PathOutsideRoot);

    let link_metadata = fixture
        .runtime
        .filesystem()
        .stat(
            &OperationContext::new(),
            StatRequest {
                path: fixture.path("link.txt"),
                follow_symlinks: false,
            },
        )
        .await
        .expect("stat link itself");
    assert_eq!(link_metadata.kind, FileKind::Symlink);

    fixture
        .runtime
        .filesystem()
        .write(
            &OperationContext::new(),
            WriteFileRequest {
                expected_generation: None,
                strategy: execution_core::WriteStrategy::AtomicReplace,
                operation_id: OperationId::generate(),
                path: fixture.path("link.txt"),
                data: BinaryData::new(b"local".to_vec()),
                condition: WriteCondition::Any,
                create_parents: false,
                follow_symlinks: false,
            },
        )
        .await
        .expect("replace final symlink without following it");
    assert_eq!(
        tokio::fs::read(&outside).await.expect("read outside"),
        b"secret"
    );
    assert_eq!(
        tokio::fs::read(&link).await.expect("read replacement"),
        b"local"
    );
}

#[tokio::test]
async fn piped_process_output_is_journaled_and_replayable() {
    let fixture = Fixture::new().await;
    let request = fixture.start_request(
        "printf 'from-out'; printf 'from-err' >&2",
        "<nul set /p \"=from-out\" & <nul set /p \"=from-err\" 1>&2",
        StdinMode::Closed,
    );
    let handle = fixture
        .runtime
        .processes()
        .start(&OperationContext::new(), request.clone())
        .await
        .expect("start process");

    let events = read_until_terminal(&fixture.runtime, &handle.execution_id).await;
    assert_eq!(events.state, ExecutionState::Exited);
    assert_eq!(events.exit_code, Some(0));
    assert_eq!(stream_bytes(&events.events, false), b"from-out");
    assert_eq!(stream_bytes(&events.events, true), b"from-err");
    assert!(matches!(
        events.events.last().map(|event| &event.event),
        Some(ProcessEventKind::Closed)
    ));

    let replay = fixture
        .runtime
        .processes()
        .read(
            &OperationContext::new(),
            ReadExecutionRequest {
                execution_id: handle.execution_id.clone(),
                supervisor_generation_id: handle.supervisor_generation_id.clone(),
                after_sequence: 0,
                max_bytes: 1024 * 1024,
                wait_ms: None,
            },
        )
        .await
        .expect("replay process events");
    assert_eq!(replay.events, events.events);

    let retried = fixture
        .runtime
        .processes()
        .start(&OperationContext::new(), request)
        .await
        .expect("retry process start");
    assert_eq!(retried.execution_id, handle.execution_id);
    assert_eq!(retried.state, ExecutionState::Exited);
}

#[tokio::test]
async fn process_input_is_deduplicated_by_write_id() {
    let fixture = Fixture::new().await;
    let handle = fixture
        .runtime
        .processes()
        .start(
            &OperationContext::new(),
            fixture.start_request(
                "IFS= read -r line; printf '<%s>' \"$line\"",
                "set /p line= & <nul set /p \"=<%line%>\"",
                StdinMode::Pipe,
            ),
        )
        .await
        .expect("start process");
    let write_id = WriteId::generate();
    let write = WriteProcessInputRequest {
        execution_id: handle.execution_id.clone(),
        supervisor_generation_id: handle.supervisor_generation_id.clone(),
        write_id,
        input: ProcessInput::Data {
            data: BinaryData::new(b"hello\n".to_vec()),
        },
    };
    let accepted = fixture
        .runtime
        .processes()
        .write(&OperationContext::new(), write.clone())
        .await
        .expect("write input");
    assert_eq!(accepted.status, ProcessInputStatus::Accepted);
    let replay = fixture
        .runtime
        .processes()
        .write(&OperationContext::new(), write)
        .await
        .expect("retry input");
    assert_eq!(replay.status, ProcessInputStatus::AlreadyAccepted);

    let events = read_until_terminal(&fixture.runtime, &handle.execution_id).await;
    assert_eq!(stream_bytes(&events.events, false), b"<hello>");
}

#[tokio::test]
async fn process_reads_honor_byte_limits_without_losing_large_events() {
    let fixture = Fixture::new().await;
    let handle = fixture
        .runtime
        .processes()
        .start(
            &OperationContext::new(),
            fixture.start_request("printf 'abcd'", "<nul set /p \"=abcd\"", StdinMode::Closed),
        )
        .await
        .expect("start process");
    let complete = read_until_terminal(&fixture.runtime, &handle.execution_id).await;

    let mut cursor = 0;
    let mut replayed = Vec::new();
    while cursor < complete.next_sequence {
        let result = fixture
            .runtime
            .processes()
            .read(
                &OperationContext::new(),
                ReadExecutionRequest {
                    execution_id: handle.execution_id.clone(),
                    supervisor_generation_id: handle.supervisor_generation_id.clone(),
                    after_sequence: cursor,
                    max_bytes: 1,
                    wait_ms: None,
                },
            )
            .await
            .expect("read bounded output");
        let output_bytes: usize = result
            .events
            .iter()
            .map(|event| match &event.event {
                ProcessEventKind::Output { data, .. } => data.len(),
                _ => 0,
            })
            .sum();
        assert!(output_bytes <= 1);
        assert!(result.next_sequence > cursor);
        cursor = result.next_sequence;
        replayed.extend(stream_bytes(&result.events, false));
    }
    assert_eq!(replayed, b"abcd");
}

#[cfg(unix)]
#[tokio::test]
async fn pty_process_can_be_resized_and_driven_incrementally() {
    let fixture = Fixture::new().await;
    let handle = fixture
        .runtime
        .processes()
        .start(
            &OperationContext::new(),
            fixture.start_request(
                "IFS= read -r line; stty size; printf '<%s>' \"$line\"",
                "",
                StdinMode::Pty {
                    columns: 80,
                    rows: 24,
                },
            ),
        )
        .await
        .expect("start PTY process");
    fixture
        .runtime
        .processes()
        .resize(
            &OperationContext::new(),
            ResizePtyRequest {
                operation_id: OperationId::generate(),
                execution_id: handle.execution_id.clone(),
                supervisor_generation_id: handle.supervisor_generation_id.clone(),
                columns: 100,
                rows: 40,
            },
        )
        .await
        .expect("resize PTY");
    fixture
        .runtime
        .processes()
        .write(
            &OperationContext::new(),
            WriteProcessInputRequest {
                execution_id: handle.execution_id.clone(),
                supervisor_generation_id: handle.supervisor_generation_id.clone(),
                write_id: WriteId::generate(),
                input: ProcessInput::Data {
                    data: BinaryData::new(b"hello\n".to_vec()),
                },
            },
        )
        .await
        .expect("write PTY input");

    let events = read_until_terminal(&fixture.runtime, &handle.execution_id).await;
    let output = stream_bytes(&events.events, false);
    let output = String::from_utf8_lossy(&output);
    assert!(output.contains("40 100"), "PTY output was {output:?}");
    assert!(output.contains("hello"), "PTY output was {output:?}");
    assert_eq!(events.state, ExecutionState::Exited);
}

#[tokio::test]
async fn timeout_and_explicit_termination_set_distinct_terminal_states() {
    let limits = SupervisorLimits {
        termination_grace_period: Duration::from_millis(20),
        ..SupervisorLimits::default()
    };
    let fixture = Fixture::with_limits(limits).await;

    let mut timed_request =
        fixture.start_request("sleep 10", "ping 127.0.0.1 -n 11 >nul", StdinMode::Closed);
    timed_request.timeout_ms = Some(30);
    let timed = fixture
        .runtime
        .processes()
        .start(&OperationContext::new(), timed_request)
        .await
        .expect("start timed process");
    let timed_events = read_until_terminal(&fixture.runtime, &timed.execution_id).await;
    assert_eq!(timed_events.state, ExecutionState::Failed);

    let terminated = fixture
        .runtime
        .processes()
        .start(
            &OperationContext::new(),
            fixture.start_request("sleep 10", "ping 127.0.0.1 -n 11 >nul", StdinMode::Closed),
        )
        .await
        .expect("start process to terminate");
    let result = fixture
        .runtime
        .processes()
        .terminate(
            &OperationContext::new(),
            TerminateExecutionRequest {
                operation_id: OperationId::generate(),
                execution_id: terminated.execution_id.clone(),
                supervisor_generation_id: terminated.supervisor_generation_id.clone(),
            },
        )
        .await
        .expect("terminate process");
    assert!(result.was_running);
    let terminated_events = read_until_terminal(&fixture.runtime, &terminated.execution_id).await;
    assert_eq!(terminated_events.state, ExecutionState::Cancelled);
}

#[tokio::test]
async fn cleanup_discards_output_without_allowing_execution_id_reuse() {
    let limits = SupervisorLimits {
        completed_execution_retention: Duration::from_millis(30),
        ..SupervisorLimits::default()
    };
    let fixture = Fixture::with_limits(limits).await;
    let request = fixture.start_request("exit 0", "exit /b 0", StdinMode::Closed);
    let handle = fixture
        .runtime
        .processes()
        .start(&OperationContext::new(), request.clone())
        .await
        .expect("start process");
    read_until_terminal(&fixture.runtime, &handle.execution_id).await;
    tokio::time::sleep(Duration::from_millis(250)).await;

    let expired = fixture
        .runtime
        .processes()
        .read(
            &OperationContext::new(),
            ReadExecutionRequest {
                execution_id: handle.execution_id.clone(),
                supervisor_generation_id: handle.supervisor_generation_id,
                after_sequence: 0,
                max_bytes: 1024,
                wait_ms: None,
            },
        )
        .await
        .expect_err("completed execution should expire");
    assert_eq!(expired.code, ExecutionErrorCode::ExecutionNotFound);

    let reused = fixture
        .runtime
        .processes()
        .start(&OperationContext::new(), request)
        .await
        .expect_err("execution ID must not be reused within a generation");
    assert_eq!(reused.code, ExecutionErrorCode::OperationConflict);
}

async fn read_until_terminal(
    runtime: &SupervisorRuntime,
    execution_id: &ExecutionId,
) -> execution_core::ReadExecutionResult {
    let generation_id = runtime.descriptor().supervisor_generation_id.clone();
    let mut events = Vec::new();
    let mut after_sequence = 0;
    for _ in 0..20 {
        let result = runtime
            .processes()
            .read(
                &OperationContext::new(),
                ReadExecutionRequest {
                    execution_id: execution_id.clone(),
                    supervisor_generation_id: generation_id.clone(),
                    after_sequence,
                    max_bytes: 1024 * 1024,
                    wait_ms: Some(1_000),
                },
            )
            .await
            .expect("read process events");
        after_sequence = result.next_sequence;
        events.extend(result.events);
        if matches!(
            result.state,
            ExecutionState::Exited
                | ExecutionState::Failed
                | ExecutionState::Cancelled
                | ExecutionState::Lost
        ) {
            return execution_core::ReadExecutionResult { events, ..result };
        }
    }
    panic!("process did not reach a terminal state");
}

fn stream_bytes(events: &[ProcessEvent], stderr: bool) -> Vec<u8> {
    let mut bytes = Vec::new();
    for event in events {
        match &event.event {
            ProcessEventKind::Output { stream, data }
                if ((*stream == execution_core::ProcessOutputStream::Stderr) == stderr) =>
            {
                bytes.extend_from_slice(data.as_slice());
            }
            _ => {}
        }
    }
    bytes
}

#[cfg(unix)]
#[tokio::test]
async fn legacy_process_requests_still_wait_for_descendant_output() {
    let fixture = Fixture::new().await;
    let context = OperationContext::with_timeout(Duration::from_secs(5));
    let request = fixture.start_request("(sleep 0.4; printf late) & exit 7", "", StdinMode::Closed);
    let encoded = serde_json::to_value(&request).unwrap();
    assert!(encoded.get("expected_generation").is_none());
    assert!(encoded.get("output_drain_timeout_ms").is_none());
    let handle = fixture
        .runtime
        .processes()
        .start(&context, request)
        .await
        .unwrap();
    tokio::time::sleep(Duration::from_millis(100)).await;
    let first = fixture
        .runtime
        .processes()
        .read(
            &context,
            ReadExecutionRequest {
                execution_id: handle.execution_id.clone(),
                supervisor_generation_id: handle.supervisor_generation_id.clone(),
                after_sequence: 0,
                max_bytes: 1024,
                wait_ms: None,
            },
        )
        .await
        .unwrap();
    assert_eq!(first.state, ExecutionState::Running);
    let mut cursor = first.next_sequence;
    let mut bytes = Vec::new();
    loop {
        let page = fixture
            .runtime
            .processes()
            .read(
                &context,
                ReadExecutionRequest {
                    execution_id: handle.execution_id.clone(),
                    supervisor_generation_id: handle.supervisor_generation_id.clone(),
                    after_sequence: cursor,
                    max_bytes: 1024,
                    wait_ms: Some(100),
                },
            )
            .await
            .unwrap();
        cursor = page.next_sequence;
        for event in &page.events {
            if let ProcessEventKind::Output { data, .. } = &event.event {
                bytes.extend_from_slice(data.as_slice());
            }
        }
        if page.state == ExecutionState::Exited {
            assert_eq!(page.exit_code, Some(7));
            assert!(
                page.events
                    .iter()
                    .any(|event| matches!(event.event, ProcessEventKind::Closed))
            );
            break;
        }
    }
    assert_eq!(bytes, b"late");
}
