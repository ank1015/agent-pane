use std::time::Duration;

use execution_contracts::{
    ApplyMutationRequest, CommandSpec, ContentSource, DiagnosticsMode, EnvironmentId,
    EnvironmentInheritance, EnvironmentVariables, ExecutionId, ExecutionPersistence,
    ExecutionPolicy, ExecutionState, FormatMode, InspectExecutionRequest, ListRequest, ListSort,
    MachineId, MutationAtomicity, MutationOperation, MutationPlan, MutationPostActions,
    NetworkMode, OperationId, PathSpec, ProcessEventKind, ProcessOutputPolicy, ReadMode,
    ReadRequest, SandboxMode, SearchKind, SearchRequest, StartExecutionRequest, StdinMode,
    TerminateExecutionRequest, TextPageRequest, WorkspaceRootId,
};
use execution_local::{LocalExecutionConfig, LocalExecutionEnvironment, LocalWorkspaceRoot};
use execution_runtime::{ExecutionEnvironment, OperationContext};
use futures_util::StreamExt;
use tempfile::TempDir;

fn id<T>(value: &str) -> T
where
    T: TryFrom<String>,
    T::Error: std::fmt::Debug,
{
    T::try_from(value.to_owned()).expect("valid test identifier")
}

fn workspace(path: &str) -> PathSpec {
    PathSpec::workspace(id::<WorkspaceRootId>("root-1"), path)
}

async fn environment() -> (TempDir, LocalExecutionEnvironment) {
    let directory = tempfile::tempdir().expect("temporary directory");
    tokio::fs::create_dir_all(directory.path().join("workspace/src"))
        .await
        .expect("create workspace");
    tokio::fs::write(
        directory.path().join("workspace/src/lib.rs"),
        "alpha\nbeta\ngamma\n",
    )
    .await
    .expect("write fixture");
    let environment = LocalExecutionEnvironment::new(LocalExecutionConfig {
        machine_id: id::<MachineId>("machine-1"),
        environment_id: id::<EnvironmentId>("host"),
        name: "Local fixture".to_owned(),
        state_directory: directory.path().join("state"),
        workspace_roots: vec![LocalWorkspaceRoot {
            id: id::<WorkspaceRootId>("root-1"),
            name: "fixture".to_owned(),
            path: directory.path().join("workspace"),
            read_only: false,
        }],
        native_grants: Vec::new(),
    })
    .await
    .expect("local environment");
    (directory, environment)
}

#[tokio::test]
async fn advertises_a_consistent_local_capability_set() {
    let (_directory, environment) = environment().await;
    environment
        .validate_capabilities()
        .expect("capabilities are consistent");
    assert_eq!(environment.descriptor().workspace_roots.len(), 1);
    assert!(environment.workspace_mutation().is_some());
    assert!(environment.process_runtime().is_some());
    assert!(environment.artifact_store().is_some());
    assert!(environment.filesystem().is_some());
}

#[tokio::test]
async fn reads_lists_and_searches_with_bounded_results() {
    let (_directory, environment) = environment().await;
    let context = OperationContext::new();
    let query = environment.workspace_query();
    let read = query
        .read(
            &context,
            ReadRequest {
                path: workspace("src/lib.rs"),
                mode: ReadMode::Text,
                page: Some(TextPageRequest {
                    start_line: Some(2),
                    max_lines: 1,
                    max_bytes: 64,
                    max_line_bytes: 64,
                    ..TextPageRequest::default()
                }),
            },
        )
        .await
        .expect("read text");
    let execution_contracts::ReadResult::Text { page } = read else {
        panic!("expected text page");
    };
    assert_eq!(page.content, "beta\n");
    assert!(page.has_more);

    let list = query
        .list(
            &context,
            ListRequest {
                path: workspace("src"),
                limit: 10,
                cursor: None,
                sort: ListSort::NameAscending,
                include_hidden: false,
                follow_symlinks: false,
            },
        )
        .await
        .expect("list directory");
    assert_eq!(list.entries[0].name, "lib.rs");

    let search = query
        .search(
            &context,
            SearchRequest {
                root: workspace("."),
                kind: SearchKind::Content,
                pattern: "beta".to_owned(),
                syntax: execution_contracts::SearchSyntax::Literal,
                include: Vec::new(),
                exclude: Vec::new(),
                ignore_mode: execution_contracts::IgnoreMode::None,
                hidden: false,
                follow_symlinks: false,
                context_before: 1,
                context_after: 1,
                max_results: 10,
                max_bytes: 1024,
                max_line_bytes: 256,
                cursor: None,
            },
        )
        .await
        .expect("search content");
    assert_eq!(search.items.len(), 1);
}

#[cfg(unix)]
#[tokio::test]
async fn rejects_symlinks_that_escape_a_workspace_root() {
    use std::os::unix::fs::symlink;

    let (directory, environment) = environment().await;
    let outside = directory.path().join("outside.txt");
    tokio::fs::write(&outside, "secret")
        .await
        .expect("write outside fixture");
    symlink(&outside, directory.path().join("workspace/escape")).expect("create escaping symlink");

    let error = environment
        .workspace_query()
        .read(
            &OperationContext::new(),
            ReadRequest {
                path: workspace("escape"),
                mode: ReadMode::Text,
                page: None,
            },
        )
        .await
        .expect_err("escaping symlink must fail");
    assert_eq!(
        error.code,
        execution_contracts::ExecutionErrorCode::PermissionDenied
    );
}

#[tokio::test]
async fn applies_mutations_idempotently() {
    let (directory, environment) = environment().await;
    let mutation = environment
        .workspace_mutation()
        .expect("mutation capability");
    let operation_id = id::<OperationId>("create-1");
    let request = ApplyMutationRequest {
        operation_id: operation_id.clone(),
        plan: MutationPlan {
            operations: vec![MutationOperation::CreateFile {
                path: workspace("created.txt"),
                content: ContentSource::Text {
                    content: "created\n".to_owned(),
                },
                create_parents: true,
            }],
            atomicity: MutationAtomicity::ValidateAll,
            post_actions: MutationPostActions {
                format: FormatMode::None,
                diagnostics: DiagnosticsMode::None,
            },
        },
    };
    let first = mutation
        .apply(&OperationContext::new(), request.clone())
        .await
        .expect("first apply");
    let second = mutation
        .apply(&OperationContext::new(), request)
        .await
        .expect("idempotent replay");
    assert_eq!(first, second);
    assert_eq!(
        tokio::fs::read_to_string(directory.path().join("workspace/created.txt"))
            .await
            .expect("created file"),
        "created\n"
    );
}

#[tokio::test]
async fn runs_a_process_and_replays_ordered_output() {
    let (_directory, environment) = environment().await;
    let runtime = environment.process_runtime().expect("process capability");
    let execution_id = id::<ExecutionId>("exec-1");
    runtime
        .start(
            &OperationContext::new(),
            StartExecutionRequest {
                operation_id: id::<OperationId>("start-1"),
                execution_id: execution_id.clone(),
                command: shell_echo_command(),
                cwd: workspace("."),
                environment: EnvironmentVariables {
                    inherit: EnvironmentInheritance::All,
                    allow: Vec::new(),
                    remove: Vec::new(),
                    set: Default::default(),
                },
                stdin: StdinMode::Closed,
                timeout_ms: Some(5_000),
                persistence: ExecutionPersistence::KeepUntilExit,
                policy: ExecutionPolicy {
                    sandbox: SandboxMode::Disabled,
                    network: NetworkMode::Inherit,
                    profile: None,
                    resource_limits: None,
                },
                output: ProcessOutputPolicy {
                    persist_full_output: true,
                    max_inline_bytes: 64 * 1024,
                    max_chunk_bytes: 1024,
                },
            },
        )
        .await
        .expect("start process");

    for _ in 0..100 {
        let status = runtime
            .inspect(
                &OperationContext::new(),
                InspectExecutionRequest {
                    execution_id: execution_id.clone(),
                },
            )
            .await
            .expect("inspect process");
        if status.state == ExecutionState::Exited {
            break;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }

    let mut events = runtime
        .attach(
            &OperationContext::new(),
            execution_contracts::AttachExecutionRequest {
                execution_id,
                after_sequence: None,
            },
        )
        .await
        .expect("attach output");
    let mut values = Vec::new();
    while let Some(event) = events.next().await {
        let event = event.expect("process event");
        let closed = matches!(event.event, ProcessEventKind::Closed);
        values.push(event);
        if closed {
            break;
        }
    }
    execution_runtime::conformance::check_process_events(&values, None).expect("ordered events");
    assert!(values.iter().any(|event| {
        matches!(
            event.event,
            ProcessEventKind::Output {
                stream: execution_contracts::ProcessOutputStream::Stdout,
                ..
            }
        )
    }));
}

#[cfg(unix)]
#[tokio::test]
async fn terminating_a_shell_kills_its_descendants() {
    let (directory, environment) = environment().await;
    let runtime = environment.process_runtime().expect("process capability");
    let execution_id = id::<ExecutionId>("process-tree-1");
    runtime
        .start(
            &OperationContext::new(),
            StartExecutionRequest {
                operation_id: id::<OperationId>("start-process-tree-1"),
                execution_id: execution_id.clone(),
                command: CommandSpec::Shell {
                    command: "sleep 60 & child=$!; printf '%s' \"$child\" > child.pid; wait"
                        .to_owned(),
                    shell: Some("/bin/sh".to_owned()),
                    login: false,
                },
                cwd: workspace("."),
                environment: EnvironmentVariables::default(),
                stdin: StdinMode::Closed,
                timeout_ms: Some(70_000),
                persistence: ExecutionPersistence::KeepUntilExit,
                policy: ExecutionPolicy {
                    sandbox: SandboxMode::Disabled,
                    network: NetworkMode::Inherit,
                    profile: None,
                    resource_limits: None,
                },
                output: ProcessOutputPolicy {
                    persist_full_output: false,
                    max_inline_bytes: 1024,
                    max_chunk_bytes: 1024,
                },
            },
        )
        .await
        .expect("start process tree");

    let pid_path = directory.path().join("workspace/child.pid");
    for _ in 0..100 {
        if tokio::fs::try_exists(&pid_path)
            .await
            .expect("check pid file")
        {
            break;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    let child_pid = tokio::fs::read_to_string(&pid_path)
        .await
        .expect("read child pid");

    runtime
        .terminate(
            &OperationContext::new(),
            TerminateExecutionRequest {
                execution_id: execution_id.clone(),
            },
        )
        .await
        .expect("terminate process tree");

    for _ in 0..100 {
        let process_gone = !std::process::Command::new("kill")
            .args(["-0", child_pid.trim()])
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .status()
            .expect("inspect child process")
            .success();
        if process_gone {
            return;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    panic!("child process {child_pid} survived execution termination");
}

#[tokio::test]
async fn runs_a_real_pty_process() {
    let (_directory, environment) = environment().await;
    let runtime = environment.process_runtime().expect("process capability");
    let execution_id = id::<ExecutionId>("pty-1");
    runtime
        .start(
            &OperationContext::new(),
            StartExecutionRequest {
                operation_id: id::<OperationId>("start-pty-1"),
                execution_id: execution_id.clone(),
                command: shell_echo_command(),
                cwd: workspace("."),
                environment: EnvironmentVariables::default(),
                stdin: StdinMode::Pty {
                    columns: 100,
                    rows: 30,
                },
                timeout_ms: Some(5_000),
                persistence: ExecutionPersistence::KeepUntilExit,
                policy: ExecutionPolicy {
                    sandbox: SandboxMode::Disabled,
                    network: NetworkMode::Inherit,
                    profile: None,
                    resource_limits: None,
                },
                output: ProcessOutputPolicy {
                    persist_full_output: true,
                    max_inline_bytes: 64 * 1024,
                    max_chunk_bytes: 1024,
                },
            },
        )
        .await
        .expect("start PTY process");

    for _ in 0..100 {
        let status = runtime
            .inspect(
                &OperationContext::new(),
                InspectExecutionRequest {
                    execution_id: execution_id.clone(),
                },
            )
            .await
            .expect("inspect PTY process");
        if status.state == ExecutionState::Exited {
            break;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    let mut events = runtime
        .attach(
            &OperationContext::new(),
            execution_contracts::AttachExecutionRequest {
                execution_id,
                after_sequence: None,
            },
        )
        .await
        .expect("attach PTY output");
    let mut saw_pty_output = false;
    while let Some(event) = events.next().await {
        let event = event.expect("PTY event");
        saw_pty_output |= matches!(
            event.event,
            ProcessEventKind::Output {
                stream: execution_contracts::ProcessOutputStream::Pty,
                ..
            }
        );
        if matches!(event.event, ProcessEventKind::Closed) {
            break;
        }
    }
    assert!(saw_pty_output);
}

fn shell_echo_command() -> CommandSpec {
    #[cfg(unix)]
    {
        CommandSpec::Argv {
            program: "/bin/sh".to_owned(),
            arguments: vec!["-c".to_owned(), "printf local-runtime".to_owned()],
        }
    }
    #[cfg(windows)]
    {
        CommandSpec::Argv {
            program: "cmd.exe".to_owned(),
            arguments: vec!["/C".to_owned(), "echo local-runtime".to_owned()],
        }
    }
}
