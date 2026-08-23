use std::collections::BTreeMap;

use async_trait::async_trait;
use execution_contracts::{
    Capability, ContinuationCursor, DirectoryEntry, EnvironmentDescriptor, EnvironmentId,
    ExecutionError, ExecutionErrorCode, ExecutionId, FileKind, FileMetadata, InspectManyRequest,
    InspectManyResult, InspectRequest, ListRequest, ListResult, ListSort, MachineId,
    OperatingSystem, PathConvention, PathSpec, ProcessEvent, ProcessEventKind, ProtocolVersion,
    ReadMode, ReadRequest, ReadResult, SearchRequest, SearchResult, TextPage, TextPageRequest,
    TimestampMs, WORKSPACE_MUTATION_CAPABILITY, WORKSPACE_QUERY_CAPABILITY, WorkspaceRoot,
    WorkspaceRootId,
};
use execution_runtime::{
    ExecutionEnvironment, ExecutionResult, OperationContext, WorkspaceQuery,
    conformance::{check_list_result, check_process_events, check_read_result},
    validate_capability_consistency,
};

fn id<T>(value: &str) -> T
where
    T: TryFrom<String>,
    T::Error: std::fmt::Debug,
{
    T::try_from(value.to_owned()).expect("valid test identifier")
}

fn path(value: &str) -> PathSpec {
    PathSpec::workspace(id::<WorkspaceRootId>("root-1"), value)
}

fn metadata(value: &str, kind: FileKind) -> FileMetadata {
    FileMetadata {
        path: path(value),
        kind,
        size: 0,
        created_at: None,
        modified_at: None,
        revision: None,
        mime_type: None,
    }
}

fn unsupported() -> ExecutionError {
    ExecutionError {
        code: ExecutionErrorCode::Unsupported,
        message: "not used by this test backend".to_owned(),
        retryable: false,
        details: BTreeMap::new(),
    }
}

struct QueryStub;

#[async_trait]
impl WorkspaceQuery for QueryStub {
    async fn inspect(
        &self,
        _context: &OperationContext,
        _request: InspectRequest,
    ) -> ExecutionResult<FileMetadata> {
        Err(unsupported())
    }

    async fn inspect_many(
        &self,
        _context: &OperationContext,
        _request: InspectManyRequest,
    ) -> ExecutionResult<InspectManyResult> {
        Err(unsupported())
    }

    async fn read(
        &self,
        _context: &OperationContext,
        _request: ReadRequest,
    ) -> ExecutionResult<ReadResult> {
        Err(unsupported())
    }

    async fn list(
        &self,
        _context: &OperationContext,
        _request: ListRequest,
    ) -> ExecutionResult<ListResult> {
        Err(unsupported())
    }

    async fn search(
        &self,
        _context: &OperationContext,
        _request: SearchRequest,
    ) -> ExecutionResult<SearchResult> {
        Err(unsupported())
    }
}

struct TestEnvironment {
    descriptor: EnvironmentDescriptor,
    query: QueryStub,
}

impl ExecutionEnvironment for TestEnvironment {
    fn descriptor(&self) -> &EnvironmentDescriptor {
        &self.descriptor
    }

    fn workspace_query(&self) -> &dyn WorkspaceQuery {
        &self.query
    }
}

fn descriptor(capability_ids: &[&str]) -> EnvironmentDescriptor {
    EnvironmentDescriptor {
        protocol_version: ProtocolVersion::V1,
        machine_id: id::<MachineId>("machine-1"),
        environment_id: id::<EnvironmentId>("host"),
        name: "Test host".to_owned(),
        operating_system: OperatingSystem::Linux,
        architecture: "x86_64".to_owned(),
        path_convention: PathConvention::Posix,
        default_shell: None,
        workspace_roots: vec![WorkspaceRoot {
            id: id::<WorkspaceRootId>("root-1"),
            name: "fixture".to_owned(),
            uri: "file:///fixture".to_owned(),
            read_only: false,
        }],
        capabilities: capability_ids
            .iter()
            .map(|capability| Capability::v1(*capability).expect("valid capability"))
            .collect(),
    }
}

#[test]
fn capability_composition_accepts_matching_runtime() {
    let environment = TestEnvironment {
        descriptor: descriptor(&[WORKSPACE_QUERY_CAPABILITY]),
        query: QueryStub,
    };

    validate_capability_consistency(&environment).expect("capabilities match");
    let object: &dyn ExecutionEnvironment = &environment;
    assert_eq!(object.descriptor().name, "Test host");
}

#[test]
fn capability_composition_rejects_advertised_missing_runtime() {
    let environment = TestEnvironment {
        descriptor: descriptor(&[WORKSPACE_QUERY_CAPABILITY, WORKSPACE_MUTATION_CAPABILITY]),
        query: QueryStub,
    };

    let error = validate_capability_consistency(&environment).expect_err("mutation is missing");
    assert!(error.issues.iter().any(|issue| {
        issue.capability == WORKSPACE_MUTATION_CAPABILITY
            && issue.message.contains("without a runtime implementation")
    }));
}

#[tokio::test]
async fn cancellation_propagates_to_child_contexts() {
    let parent = OperationContext::new();
    let child = parent.child();
    parent.cancel();

    child.cancelled().await;
    assert!(child.is_cancelled());
}

#[test]
fn conformance_checks_bounded_workspace_results() {
    let read_request = ReadRequest {
        path: path("src/lib.rs"),
        mode: ReadMode::Text,
        page: Some(TextPageRequest {
            max_lines: 1,
            max_bytes: 8,
            max_line_bytes: 8,
            ..TextPageRequest::default()
        }),
    };
    let invalid_read = ReadResult::Text {
        page: TextPage {
            metadata: metadata("src/lib.rs", FileKind::File),
            content: "one\ntwo\n".to_owned(),
            start_line: 1,
            end_line: 2,
            has_more: false,
            next_line: None,
            cursor: None,
            total_lines: None,
            lines_truncated: false,
        },
    };
    assert_eq!(
        check_read_result(&read_request, &invalid_read)
            .expect_err("too many lines")
            .path,
        "read.content"
    );

    let list_request = ListRequest {
        path: path("src"),
        limit: 1,
        cursor: None,
        sort: ListSort::NameAscending,
        include_hidden: false,
        follow_symlinks: false,
    };
    let invalid_list = ListResult {
        directory: metadata("src", FileKind::Directory),
        entries: vec![DirectoryEntry {
            path: path("src/lib.rs"),
            name: "lib.rs".to_owned(),
            kind: FileKind::File,
            size: None,
            modified_at: None,
        }],
        truncated: true,
        cursor: None,
    };
    assert_eq!(
        check_list_result(&list_request, &invalid_list)
            .expect_err("truncated page needs a cursor")
            .path,
        "list.pagination"
    );
}

#[test]
fn conformance_checks_process_resumption_and_ordering() {
    let execution_id = id::<ExecutionId>("exec-1");
    let events = vec![
        ProcessEvent {
            execution_id: execution_id.clone(),
            sequence: 11,
            timestamp: TimestampMs(1),
            event: ProcessEventKind::Started,
        },
        ProcessEvent {
            execution_id: execution_id.clone(),
            sequence: 12,
            timestamp: TimestampMs(2),
            event: ProcessEventKind::Exited {
                exit_code: 0,
                sandbox_denied: false,
            },
        },
        ProcessEvent {
            execution_id,
            sequence: 13,
            timestamp: TimestampMs(3),
            event: ProcessEventKind::Closed,
        },
    ];

    check_process_events(&events, Some(10)).expect("ordered resumed events");
    assert_eq!(
        check_process_events(&events, Some(11))
            .expect_err("resume sequence must be exclusive")
            .path,
        "process.sequence"
    );
}

#[test]
fn opaque_cursor_type_remains_available_to_adapter_tests() {
    let cursor = id::<ContinuationCursor>("next-page");
    assert_eq!(cursor.as_str(), "next-page");
}
