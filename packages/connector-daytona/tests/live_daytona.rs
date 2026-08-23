use std::time::Duration;

use base64::{Engine, engine::general_purpose::STANDARD};
use connector_daytona::{
    DaytonaConnectionConfig, DaytonaEnvironmentConfig, DaytonaExecutionEnvironment,
    DaytonaWorkspaceRoot,
};
use execution_contracts::{
    ApplyMutationRequest, AttachExecutionRequest, CommandSpec, CommitMutationRequest,
    ContentSource, DiagnosticsMode, EnvironmentId, EnvironmentInheritance, EnvironmentVariables,
    ExecutionId, ExecutionPersistence, ExecutionPolicy, ExecutionState, FormatMode,
    GetArtifactMetadataRequest, InspectExecutionRequest, MachineId, MutationAtomicity,
    MutationOperation, MutationPlan, MutationPostActions, NetworkMode, OccurrencePolicy,
    OpenArtifactRequest, OperationId, PathSpec, PrepareMutationRequest, ProcessEventKind,
    ProcessInputStatus, ProcessOutputPolicy, ReadMode, ReadRequest, RemovePathRequest,
    ResizePtyRequest, SandboxMode, SearchKind, SearchRequest, StartExecutionRequest, StdinMode,
    TextReplacement, WorkspaceRootId, WriteProcessInputRequest,
};
use execution_runtime::{ExecutionEnvironment, OperationContext};
use futures_util::StreamExt;

mod full_environment {
    include!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../execution-runtime/test-support/full_environment.rs"
    ));
}

fn id<T>(value: &str) -> T
where
    T: TryFrom<String>,
    T::Error: std::fmt::Debug,
{
    T::try_from(value.to_owned()).expect("valid test identifier")
}

fn workspace(path: &str) -> PathSpec {
    PathSpec::workspace(id::<WorkspaceRootId>("root"), path)
}

fn make_environment() -> DaytonaExecutionEnvironment {
    let toolbox_url = std::env::var("DAYTONA_TOOLBOX_URL").expect("DAYTONA_TOOLBOX_URL");
    let api_key = std::env::var("DAYTONA_API_KEY").expect("DAYTONA_API_KEY");
    let workspace_root =
        std::env::var("DAYTONA_WORKSPACE_ROOT").unwrap_or_else(|_| "/home/daytona".to_owned());
    let state_directory = format!("{workspace_root}/.agent-pane-live-test");
    DaytonaExecutionEnvironment::connect(
        DaytonaConnectionConfig::new(
            url::Url::parse(&toolbox_url).expect("valid DAYTONA_TOOLBOX_URL"),
            api_key,
        ),
        DaytonaEnvironmentConfig {
            machine_id: id::<MachineId>("daytona-live"),
            environment_id: id::<EnvironmentId>("sandbox"),
            name: "Daytona live test".to_owned(),
            state_directory,
            workspace_roots: vec![DaytonaWorkspaceRoot {
                id: id::<WorkspaceRootId>("root"),
                name: "home".to_owned(),
                path: workspace_root,
                read_only: false,
            }],
            native_grants: Vec::new(),
        },
    )
    .expect("connect Daytona environment")
}

#[tokio::test]
#[ignore = "requires DAYTONA_TOOLBOX_URL and DAYTONA_API_KEY"]
async fn full_connector_smoke_test_and_process_recovery() {
    let environment = make_environment();
    let context = OperationContext::new();
    let fixture = format!("agent-pane-smoke-{}", uuid::Uuid::now_v7());
    let apply_operation = id::<OperationId>(&format!("create-{}", uuid::Uuid::now_v7()));
    let fixture_path = |child: &str| workspace(&format!("{fixture}/{child}"));

    let mutation = environment
        .workspace_mutation()
        .expect("workspace mutation capability");
    mutation
        .apply(
            &context,
            ApplyMutationRequest {
                operation_id: apply_operation,
                plan: MutationPlan {
                    operations: vec![MutationOperation::CreateFile {
                        path: fixture_path("hello.txt"),
                        content: ContentSource::Text {
                            content: "alpha\nbeta\ngamma\n".to_owned(),
                        },
                        create_parents: true,
                    }],
                    atomicity: MutationAtomicity::ValidateAll,
                    post_actions: MutationPostActions {
                        format: FormatMode::None,
                        diagnostics: DiagnosticsMode::None,
                    },
                },
            },
        )
        .await
        .expect("apply mutation");

    let read = environment
        .workspace_query()
        .read(
            &context,
            ReadRequest {
                path: fixture_path("hello.txt"),
                mode: ReadMode::Text,
                page: None,
            },
        )
        .await
        .expect("read fixture");
    let execution_contracts::ReadResult::Text { page } = read else {
        panic!("expected text result");
    };
    assert_eq!(page.content, "alpha\nbeta\ngamma\n");

    let search = environment
        .workspace_query()
        .search(
            &context,
            SearchRequest {
                root: workspace(&fixture),
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
        .expect("search fixture");
    assert_eq!(search.items.len(), 1);

    let prepared = mutation
        .prepare(
            &context,
            PrepareMutationRequest {
                plan: MutationPlan {
                    operations: vec![MutationOperation::ReplaceText {
                        path: fixture_path("hello.txt"),
                        replacements: vec![TextReplacement {
                            old_text: "beta".to_owned(),
                            new_text: "BETA".to_owned(),
                            occurrence: OccurrencePolicy::Unique,
                        }],
                        preserve_line_endings: true,
                        preserve_utf8_bom: true,
                        expected_revision: None,
                    }],
                    atomicity: MutationAtomicity::ValidateAll,
                    post_actions: MutationPostActions::default(),
                },
            },
        )
        .await
        .expect("prepare mutation");
    assert_eq!(prepared.previews.len(), 1);
    mutation
        .commit(
            &context,
            CommitMutationRequest {
                prepared_id: prepared.prepared_id,
                operation_id: id::<OperationId>(&format!("commit-{}", uuid::Uuid::now_v7())),
            },
        )
        .await
        .expect("commit prepared mutation");

    let execution_id = id::<ExecutionId>(&format!("live-{}", uuid::Uuid::now_v7()));
    environment
        .process_runtime()
        .expect("process runtime")
        .start(
            &context,
            StartExecutionRequest {
                operation_id: id::<OperationId>("live-start"),
                execution_id: execution_id.clone(),
                command: CommandSpec::Argv {
                    program: "/bin/sh".to_owned(),
                    arguments: vec![
                        "-c".to_owned(),
                        "printf first; sleep 1; printf second".to_owned(),
                    ],
                },
                cwd: workspace(&fixture),
                environment: EnvironmentVariables {
                    inherit: EnvironmentInheritance::All,
                    allow: Vec::new(),
                    remove: Vec::new(),
                    set: Default::default(),
                },
                stdin: StdinMode::Closed,
                timeout_ms: Some(10_000),
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

    let mut terminal_status = None;
    for _ in 0..100 {
        let status = environment
            .process_runtime()
            .expect("process runtime")
            .inspect(
                &context,
                InspectExecutionRequest {
                    execution_id: execution_id.clone(),
                },
            )
            .await
            .expect("inspect process");
        if status.state == ExecutionState::Failed {
            panic!("process failed: {status:?}");
        }
        if status.state == ExecutionState::Exited && status.full_output_artifact.is_some() {
            assert_eq!(status.state, ExecutionState::Exited);
            terminal_status = Some(status);
            break;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    let terminal_status = terminal_status.expect("process reached a terminal state");
    assert!(terminal_status.full_output_artifact.is_some());

    // Constructing a second connector instance simulates losing all in-memory
    // connector state. Attach must hydrate from the sandbox-side journal.
    let recovered = make_environment();
    let mut stream = recovered
        .process_runtime()
        .expect("process runtime")
        .attach(
            &context,
            AttachExecutionRequest {
                execution_id,
                after_sequence: None,
            },
        )
        .await
        .expect("recover process journal");
    let mut output = Vec::new();
    while let Some(event) = stream.next().await {
        let event = event.expect("valid process event");
        if let ProcessEventKind::Output { data, .. } = &event.event {
            output.extend_from_slice(&STANDARD.decode(&data.0).expect("base64 output"));
        }
        if matches!(event.event, ProcessEventKind::Closed) {
            break;
        }
    }
    assert_eq!(
        String::from_utf8(output).expect("utf8 output"),
        "firstsecond"
    );

    let artifact_id = terminal_status
        .full_output_artifact
        .expect("persisted process output artifact");
    let artifact_store = recovered.artifact_store().expect("artifact store");
    let metadata = artifact_store
        .metadata(
            &context,
            GetArtifactMetadataRequest {
                artifact_id: artifact_id.clone(),
            },
        )
        .await
        .expect("process output metadata");
    assert_eq!(metadata.size, 11);
    let mut chunks = artifact_store
        .open(
            &context,
            OpenArtifactRequest {
                artifact_id,
                offset: 0,
                max_bytes: 4,
            },
        )
        .await
        .expect("open process output artifact");
    let mut artifact_output = Vec::new();
    while let Some(chunk) = chunks.next().await {
        let chunk = chunk.expect("artifact chunk");
        artifact_output.extend_from_slice(&STANDARD.decode(chunk.data.0).expect("base64 chunk"));
    }
    assert_eq!(artifact_output, b"firstsecond");

    recovered
        .filesystem()
        .expect("filesystem capability")
        .remove(
            &context,
            RemovePathRequest {
                path: workspace(&fixture),
                recursive: true,
                force: true,
                expected_revision: None,
            },
        )
        .await
        .expect("remove fixture");
}

#[tokio::test]
#[ignore = "requires DAYTONA_TOOLBOX_URL and DAYTONA_API_KEY"]
async fn controls_a_pty_and_deduplicates_process_input() {
    let environment = make_environment();
    let context = OperationContext::new();
    let execution_id = id::<ExecutionId>(&format!("pty-{}", uuid::Uuid::now_v7()));
    let runtime = environment.process_runtime().expect("process runtime");
    runtime
        .start(
            &context,
            StartExecutionRequest {
                operation_id: id::<OperationId>(&format!("start-{}", uuid::Uuid::now_v7())),
                execution_id: execution_id.clone(),
                command: CommandSpec::Argv {
                    program: "/bin/sh".to_owned(),
                    arguments: vec![
                        "-c".to_owned(),
                        "sleep 3; stty size; read line; printf 'received:%s' \"$line\"".to_owned(),
                    ],
                },
                cwd: workspace("."),
                environment: EnvironmentVariables::default(),
                stdin: StdinMode::Pty {
                    columns: 80,
                    rows: 24,
                },
                timeout_ms: Some(10_000),
                persistence: ExecutionPersistence::KeepUntilExit,
                policy: ExecutionPolicy {
                    sandbox: SandboxMode::Disabled,
                    network: NetworkMode::Inherit,
                    profile: None,
                    resource_limits: None,
                },
                output: ProcessOutputPolicy {
                    persist_full_output: false,
                    max_inline_bytes: 64 * 1024,
                    max_chunk_bytes: 1024,
                },
            },
        )
        .await
        .expect("start PTY process");

    for _ in 0..50 {
        let status = runtime
            .inspect(
                &context,
                InspectExecutionRequest {
                    execution_id: execution_id.clone(),
                },
            )
            .await
            .expect("inspect PTY process");
        if status.state == ExecutionState::Running {
            break;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    runtime
        .resize_pty(
            &context,
            ResizePtyRequest {
                execution_id: execution_id.clone(),
                columns: 100,
                rows: 30,
            },
        )
        .await
        .expect("resize PTY");
    let write_id = id::<OperationId>(&format!("write-{}", uuid::Uuid::now_v7()));
    let input = WriteProcessInputRequest {
        execution_id: execution_id.clone(),
        write_id,
        data: execution_contracts::Base64Data(STANDARD.encode(b"hello\n")),
    };
    assert_eq!(
        runtime
            .write_input(&context, input.clone())
            .await
            .expect("write PTY input")
            .status,
        ProcessInputStatus::Accepted
    );
    assert_eq!(
        runtime
            .write_input(&context, input)
            .await
            .expect("deduplicate PTY input")
            .status,
        ProcessInputStatus::Accepted
    );

    let mut stream = runtime
        .attach(
            &context,
            AttachExecutionRequest {
                execution_id,
                after_sequence: None,
            },
        )
        .await
        .expect("attach PTY");
    let mut events = Vec::new();
    let mut output = Vec::new();
    while let Some(event) = stream.next().await {
        let event = event.expect("valid PTY event");
        if let ProcessEventKind::Output { data, .. } = &event.event {
            output.extend_from_slice(&STANDARD.decode(&data.0).expect("base64 PTY output"));
        }
        let closed = matches!(event.event, ProcessEventKind::Closed);
        events.push(event);
        if closed {
            break;
        }
    }
    execution_runtime::conformance::check_process_events(&events, None)
        .expect("ordered PTY process events");
    let output = String::from_utf8_lossy(&output);
    assert!(
        output.contains("30 100"),
        "resized PTY output was {output:?}"
    );
    assert!(
        output.contains("received:hello"),
        "interactive PTY output was {output:?}"
    );
}

#[tokio::test]
#[ignore = "requires DAYTONA_TOOLBOX_URL and DAYTONA_API_KEY"]
async fn exercises_every_advertised_interface_method() {
    full_environment::exercise(&make_environment()).await;
}
