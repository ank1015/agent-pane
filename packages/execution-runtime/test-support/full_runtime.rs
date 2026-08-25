use std::time::Duration;

use base64::{Engine, engine::general_purpose::STANDARD};
use execution_contracts::{
    AbortMutationRequest, ApplyMutationRequest, AttachExecutionRequest, Base64Data, BinaryContent,
    ByteRange, CommandSpec, CommitMutationRequest, ContentSource, CopyPathRequest,
    CreateDirectoryRequest, EnvironmentInheritance, EnvironmentVariables, ExecutionErrorCode,
    ExecutionId, ExecutionPersistence, ExecutionPolicy, ExecutionState, FormatMode, IgnoreMode,
    InspectExecutionRequest, InspectManyRequest, InspectRequest, ItemOutcome, ListRequest,
    ListSort, MovePathRequest, MutationAtomicity, MutationOperation, MutationPlan,
    MutationPostActions, NetworkMode, OccurrencePolicy, OperationId, PatchMatchPolicy, PathSpec,
    PrepareMutationRequest, ProcessEventKind, ProcessOutputPolicy, ProcessSignal, ReadBytesRequest,
    ReadMode, ReadRequest, ReadResult, RemovePathRequest, SandboxMode, SearchItem, SearchKind,
    SearchRequest, SearchSyntax, SignalExecutionRequest, StartExecutionRequest, StdinMode,
    TerminateExecutionRequest, TextPageRequest, TextPatchHunk, TextReplacement, WalkRequest,
    WorkspaceRootId, WriteBytesRequest, WriteCondition,
};
use execution_runtime::{ExecutionRuntime, OperationContext};
use futures_util::StreamExt;

fn id<T>(value: &str) -> T
where
    T: TryFrom<String>,
    T::Error: std::fmt::Debug,
{
    T::try_from(value.to_owned()).expect("valid conformance identifier")
}

fn workspace(path: &str) -> PathSpec {
    PathSpec::workspace(id::<WorkspaceRootId>("root"), path)
}

fn operation(prefix: &str) -> OperationId {
    id::<OperationId>(&format!("{prefix}-{}", uuid::Uuid::now_v7()))
}

fn process_request(
    execution_id: ExecutionId,
    command: CommandSpec,
    timeout_ms: Option<u64>,
) -> StartExecutionRequest {
    StartExecutionRequest {
        operation_id: operation("start"),
        execution_id,
        command,
        cwd: workspace("."),
        environment: EnvironmentVariables {
            inherit: EnvironmentInheritance::All,
            allow: Vec::new(),
            remove: Vec::new(),
            set: Default::default(),
        },
        stdin: StdinMode::Closed,
        timeout_ms,
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
    }
}

async fn wait_until_running(
    runtime: &dyn execution_runtime::ProcessRuntime,
    context: &OperationContext,
    execution_id: &ExecutionId,
) {
    for _ in 0..200 {
        let status = runtime
            .inspect(
                context,
                InspectExecutionRequest {
                    execution_id: execution_id.clone(),
                },
            )
            .await
            .expect("inspect process while waiting for running state");
        if status.state == ExecutionState::Running {
            return;
        }
        assert!(
            matches!(
                status.state,
                ExecutionState::Queued | ExecutionState::Starting
            ),
            "process became terminal before reaching running state: {status:?}"
        );
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    panic!("process did not reach running state");
}

async fn wait_until_terminal(
    runtime: &dyn execution_runtime::ProcessRuntime,
    context: &OperationContext,
    execution_id: &ExecutionId,
) -> execution_contracts::ExecutionStatus {
    for _ in 0..300 {
        let status = runtime
            .inspect(
                context,
                InspectExecutionRequest {
                    execution_id: execution_id.clone(),
                },
            )
            .await
            .expect("inspect process while waiting for terminal state");
        if !matches!(
            status.state,
            ExecutionState::Queued | ExecutionState::Starting | ExecutionState::Running
        ) {
            return status;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    panic!("process did not reach a terminal state");
}

pub async fn exercise(runtime: &dyn ExecutionRuntime) {
    let context = OperationContext::new();
    let fixture = format!("agent-pane-interface-{}", uuid::Uuid::now_v7());
    let at = |child: &str| workspace(&format!("{fixture}/{child}"));
    let query = runtime.workspace_query();
    let mutation = runtime
        .workspace_mutation()
        .expect("workspace mutation capability");
    let filesystem = runtime.filesystem().expect("filesystem capability");

    filesystem
        .create_directory(
            &context,
            CreateDirectoryRequest {
                path: at("raw/nested"),
                recursive: true,
            },
        )
        .await
        .expect("create primitive directory");
    let first_write = filesystem
        .write_bytes(
            &context,
            WriteBytesRequest {
                path: at("raw/nested/data.bin"),
                content: ContentSource::Base64 {
                    data: Base64Data(STANDARD.encode(b"zero-one-two")),
                    mime_type: Some("application/octet-stream".to_owned()),
                },
                condition: WriteCondition::MustNotExist,
                create_parents: true,
                atomic_replace: true,
                follow_symlinks: true,
            },
        )
        .await
        .expect("conditionally write primitive file");
    assert!(!first_write.existed);

    let query_metadata = query
        .inspect(
            &context,
            InspectRequest {
                path: at("raw/nested/data.bin"),
                follow_symlinks: true,
            },
        )
        .await
        .expect("workspace inspect");
    let filesystem_metadata = filesystem
        .inspect(
            &context,
            InspectRequest {
                path: at("raw/nested/data.bin"),
                follow_symlinks: true,
            },
        )
        .await
        .expect("filesystem inspect");
    assert_eq!(query_metadata.revision, filesystem_metadata.revision);

    let many = query
        .inspect_many(
            &context,
            InspectManyRequest {
                paths: vec![at("raw/nested/data.bin"), at("missing")],
                follow_symlinks: true,
            },
        )
        .await
        .expect("inspect many");
    assert_eq!(many.items.len(), 2);
    assert!(matches!(many.items[0], ItemOutcome::Success { .. }));
    assert!(matches!(many.items[1], ItemOutcome::Error { .. }));

    let ranged = filesystem
        .read_bytes(
            &context,
            ReadBytesRequest {
                path: at("raw/nested/data.bin"),
                range: Some(ByteRange {
                    offset: 5,
                    length: 3,
                }),
                follow_symlinks: true,
            },
        )
        .await
        .expect("ranged primitive read");
    let BinaryContent::Base64 { data, .. } = ranged.content else {
        panic!("small ranged read should be inline");
    };
    assert_eq!(STANDARD.decode(data.0).expect("base64 ranged read"), b"one");

    let stale_revision = first_write.revision.clone();
    let second_write = filesystem
        .write_bytes(
            &context,
            WriteBytesRequest {
                path: at("raw/nested/data.bin"),
                content: ContentSource::Text {
                    content: "updated-data".to_owned(),
                },
                condition: WriteCondition::MatchRevision {
                    revision: stale_revision.clone(),
                },
                create_parents: false,
                atomic_replace: true,
                follow_symlinks: true,
            },
        )
        .await
        .expect("revision-matched primitive write");
    assert!(second_write.existed);
    let stale_error = filesystem
        .write_bytes(
            &context,
            WriteBytesRequest {
                path: at("raw/nested/data.bin"),
                content: ContentSource::Text {
                    content: "must-not-commit".to_owned(),
                },
                condition: WriteCondition::MatchRevision {
                    revision: stale_revision,
                },
                create_parents: false,
                atomic_replace: true,
                follow_symlinks: true,
            },
        )
        .await
        .expect_err("stale revision must fail");
    assert_eq!(stale_error.code, ExecutionErrorCode::StaleRevision);

    filesystem
        .copy_path(
            &context,
            CopyPathRequest {
                source: at("raw/nested/data.bin"),
                destination: at("raw/copied.txt"),
                recursive: false,
                overwrite: false,
            },
        )
        .await
        .expect("primitive copy");
    filesystem
        .move_path(
            &context,
            MovePathRequest {
                source: at("raw/copied.txt"),
                destination: at("raw/moved.txt"),
                overwrite: false,
                expected_source_revision: None,
            },
        )
        .await
        .expect("primitive move");

    let raw_list = filesystem
        .list_raw(
            &context,
            ListRequest {
                path: at("raw"),
                limit: 100,
                cursor: None,
                sort: ListSort::NameAscending,
                include_hidden: true,
                follow_symlinks: false,
            },
        )
        .await
        .expect("primitive raw list");
    assert!(
        raw_list
            .entries
            .iter()
            .any(|entry| entry.name == "moved.txt")
    );
    let workspace_list = query
        .list(
            &context,
            ListRequest {
                path: at("raw"),
                limit: 1,
                cursor: None,
                sort: ListSort::NameAscending,
                include_hidden: true,
                follow_symlinks: false,
            },
        )
        .await
        .expect("paginated workspace list");
    execution_runtime::conformance::check_list_result(
        &ListRequest {
            path: at("raw"),
            limit: 1,
            cursor: None,
            sort: ListSort::NameAscending,
            include_hidden: true,
            follow_symlinks: false,
        },
        &workspace_list,
    )
    .expect("valid list response");
    assert!(workspace_list.truncated);

    let walked = filesystem
        .walk(
            &context,
            WalkRequest {
                root: at("raw"),
                max_depth: 4,
                max_entries: 100,
                follow_directory_symlinks: false,
                include_hidden: true,
                cursor: None,
            },
        )
        .await
        .expect("primitive walk");
    assert!(walked.entries.iter().any(|entry| entry.name == "data.bin"));
    let path_search = query
        .search(
            &context,
            SearchRequest {
                root: at("raw"),
                kind: SearchKind::Paths,
                pattern: "*.txt".to_owned(),
                syntax: SearchSyntax::Glob,
                include: Vec::new(),
                exclude: Vec::new(),
                ignore_mode: IgnoreMode::None,
                hidden: true,
                follow_symlinks: false,
                context_before: 0,
                context_after: 0,
                max_results: 100,
                max_bytes: 16 * 1024,
                max_line_bytes: 1024,
                cursor: None,
            },
        )
        .await
        .expect("workspace path search");
    assert!(path_search.items.iter().any(|item| matches!(
        item,
        SearchItem::Path { entry } if entry.name == "moved.txt"
    )));

    let put_request = ApplyMutationRequest {
        operation_id: operation("put"),
        plan: MutationPlan {
            operations: vec![MutationOperation::PutFile {
                path: at("mutations/source.txt"),
                content: ContentSource::Text {
                    content: "alpha\nbeta\n".to_owned(),
                },
                create_parents: true,
                preserve_utf8_bom: true,
                expected_revision: None,
            }],
            atomicity: MutationAtomicity::ValidateAll,
            post_actions: MutationPostActions::default(),
        },
    };
    let first_put = mutation
        .apply(&context, put_request.clone())
        .await
        .expect("put-file mutation");
    let replayed_put = mutation
        .apply(&context, put_request)
        .await
        .expect("idempotent mutation replay");
    assert_eq!(first_put, replayed_put);
    mutation
        .apply(
            &context,
            ApplyMutationRequest {
                operation_id: operation("create"),
                plan: MutationPlan {
                    operations: vec![MutationOperation::CreateFile {
                        path: at("mutations/created.txt"),
                        content: ContentSource::Text {
                            content: "created\n".to_owned(),
                        },
                        create_parents: true,
                    }],
                    atomicity: MutationAtomicity::ValidateAll,
                    post_actions: MutationPostActions::default(),
                },
            },
        )
        .await
        .expect("create-file mutation");
    mutation
        .apply(
            &context,
            ApplyMutationRequest {
                operation_id: operation("replace"),
                plan: MutationPlan {
                    operations: vec![MutationOperation::ReplaceText {
                        path: at("mutations/source.txt"),
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
        .expect("replace-text mutation");
    mutation
        .apply(
            &context,
            ApplyMutationRequest {
                operation_id: operation("patch"),
                plan: MutationPlan {
                    operations: vec![MutationOperation::ApplyTextPatch {
                        path: at("mutations/source.txt"),
                        hunks: vec![TextPatchHunk {
                            context_hint: None,
                            old_lines: vec!["alpha".to_owned(), "BETA".to_owned()],
                            new_lines: vec![
                                "ALPHA".to_owned(),
                                "BETA".to_owned(),
                                "gamma".to_owned(),
                            ],
                            end_of_file: true,
                        }],
                        match_policy: PatchMatchPolicy::Exact,
                        preserve_line_endings: true,
                        preserve_utf8_bom: true,
                        expected_revision: None,
                    }],
                    atomicity: MutationAtomicity::ValidateAll,
                    post_actions: MutationPostActions {
                        format: FormatMode::None,
                        diagnostics: execution_contracts::DiagnosticsMode::None,
                    },
                },
            },
        )
        .await
        .expect("text-patch mutation");
    mutation
        .apply(
            &context,
            ApplyMutationRequest {
                operation_id: operation("copy-move-remove"),
                plan: MutationPlan {
                    operations: vec![
                        MutationOperation::Copy {
                            source: at("mutations/source.txt"),
                            destination: at("mutations/copied.txt"),
                            recursive: false,
                            overwrite: false,
                        },
                        MutationOperation::Move {
                            source: at("mutations/created.txt"),
                            destination: at("mutations/moved.txt"),
                            overwrite: false,
                            expected_source_revision: None,
                        },
                        MutationOperation::Remove {
                            path: at("raw/moved.txt"),
                            recursive: false,
                            force: false,
                            expected_revision: None,
                        },
                    ],
                    atomicity: MutationAtomicity::ValidateAll,
                    post_actions: MutationPostActions::default(),
                },
            },
        )
        .await
        .expect("copy/move/remove mutation variants");

    let page_request = ReadRequest {
        path: at("mutations/source.txt"),
        mode: ReadMode::Text,
        page: Some(TextPageRequest {
            start_line: Some(1),
            cursor: None,
            max_lines: 1,
            max_bytes: 1024,
            max_line_bytes: 256,
            include_total_lines: true,
        }),
    };
    let page = query
        .read(&context, page_request.clone())
        .await
        .expect("bounded workspace read");
    execution_runtime::conformance::check_read_result(&page_request, &page)
        .expect("valid bounded read response");
    let ReadResult::Text { page } = page else {
        panic!("text mutation result should read as text");
    };
    assert_eq!(page.content, "ALPHA\n");
    assert!(page.has_more);

    let aborted = mutation
        .prepare(
            &context,
            PrepareMutationRequest {
                plan: MutationPlan {
                    operations: vec![MutationOperation::PutFile {
                        path: at("mutations/aborted.txt"),
                        content: ContentSource::Text {
                            content: "never committed".to_owned(),
                        },
                        create_parents: true,
                        preserve_utf8_bom: true,
                        expected_revision: None,
                    }],
                    atomicity: MutationAtomicity::ValidateAll,
                    post_actions: MutationPostActions::default(),
                },
            },
        )
        .await
        .expect("prepare mutation for abort");
    mutation
        .abort(
            &context,
            AbortMutationRequest {
                prepared_id: aborted.prepared_id.clone(),
            },
        )
        .await
        .expect("abort mutation");
    let aborted_commit = mutation
        .commit(
            &context,
            CommitMutationRequest {
                prepared_id: aborted.prepared_id,
                operation_id: operation("aborted-commit"),
            },
        )
        .await
        .expect_err("aborted mutation cannot be committed");
    assert_eq!(aborted_commit.code, ExecutionErrorCode::NotFound);

    let runtime = runtime.process_runtime().expect("process runtime");
    let signal_id = id::<ExecutionId>(&format!("signal-{}", uuid::Uuid::now_v7()));
    runtime
        .start(
            &context,
            process_request(
                signal_id.clone(),
                CommandSpec::Argv {
                    program: "python3".to_owned(),
                    arguments: vec![
                        "-u".to_owned(),
                        "-c".to_owned(),
                        "import signal,time,sys; signal.signal(signal.SIGUSR1, lambda *_: sys.exit(7)); print('ready', flush=True); time.sleep(60)".to_owned(),
                    ],
                },
                Some(70_000),
            ),
        )
        .await
        .expect("start signal target");
    wait_until_running(runtime, &context, &signal_id).await;
    tokio::time::sleep(Duration::from_secs(1)).await;
    runtime
        .signal(
            &context,
            SignalExecutionRequest {
                execution_id: signal_id.clone(),
                signal: ProcessSignal::User1,
            },
        )
        .await
        .expect("signal process");
    let signalled = wait_until_terminal(runtime, &context, &signal_id).await;
    assert_eq!(signalled.state, ExecutionState::Exited);
    assert_eq!(signalled.exit_code, Some(7));

    let terminate_id = id::<ExecutionId>(&format!("terminate-{}", uuid::Uuid::now_v7()));
    let terminate_request = process_request(
        terminate_id.clone(),
        CommandSpec::Shell {
            command: "sleep 60".to_owned(),
            shell: Some("/bin/sh".to_owned()),
            login: false,
        },
        Some(70_000),
    );
    runtime
        .start(&context, terminate_request.clone())
        .await
        .expect("start termination target");
    runtime
        .start(&context, terminate_request)
        .await
        .expect("idempotent repeated process start");
    wait_until_running(runtime, &context, &terminate_id).await;
    let terminated = runtime
        .terminate(
            &context,
            TerminateExecutionRequest {
                execution_id: terminate_id.clone(),
            },
        )
        .await
        .expect("terminate process");
    assert!(terminated.was_running);
    let terminal_status = wait_until_terminal(runtime, &context, &terminate_id).await;
    assert!(!matches!(
        terminal_status.state,
        ExecutionState::Queued | ExecutionState::Starting | ExecutionState::Running
    ));

    let mut terminal_events = runtime
        .attach(
            &context,
            AttachExecutionRequest {
                execution_id: terminate_id,
                after_sequence: None,
            },
        )
        .await
        .expect("attach to terminated process");
    let mut events = Vec::new();
    while let Some(event) = terminal_events.next().await {
        let event = event.expect("valid terminal process event");
        let closed = matches!(
            event.event,
            ProcessEventKind::Closed | ProcessEventKind::Failed { .. }
        );
        events.push(event);
        if closed {
            break;
        }
    }
    execution_runtime::conformance::check_process_events(&events, None)
        .expect("ordered terminal process events");

    filesystem
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
        .expect("remove conformance fixture");
}
