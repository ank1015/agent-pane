use execution_core::{
    ExecutionError, ExecutionErrorCode, ExecutionResult, ExecutionRuntime, OperationContext,
};

use crate::{Operation, OperationResult, PROTOCOL_VERSION, RequestEnvelope, ResponseEnvelope};

/// Dispatches one already-versioned operation against an execution runtime.
pub async fn dispatch_operation(
    runtime: &dyn ExecutionRuntime,
    context: &OperationContext,
    operation: Operation,
) -> ExecutionResult<OperationResult> {
    operation.validate().map_err(ExecutionError::from)?;
    context.checkpoint()?;

    match operation {
        Operation::Describe => Ok(OperationResult::HostDescriptor(
            runtime.descriptor().clone(),
        )),
        Operation::FilesystemStat(request) => runtime
            .filesystem()
            .stat(context, request)
            .await
            .map(OperationResult::FileMetadata),
        Operation::FilesystemRead(request) => runtime
            .filesystem()
            .read(context, request)
            .await
            .map(OperationResult::ReadFile),
        Operation::FilesystemWrite(request) => runtime
            .filesystem()
            .write(context, request)
            .await
            .map(OperationResult::WriteFile),
        Operation::FilesystemCreateDirectory(request) => runtime
            .filesystem()
            .create_directory(context, request)
            .await
            .map(|()| OperationResult::Unit),
        Operation::FilesystemRemove(request) => runtime
            .filesystem()
            .remove(context, request)
            .await
            .map(OperationResult::RemovePath),
        Operation::FilesystemList(request) => runtime
            .filesystem()
            .list(context, request)
            .await
            .map(OperationResult::ListDirectory),
        Operation::ProcessStart(request) => runtime
            .processes()
            .start(context, request)
            .await
            .map(OperationResult::ExecutionHandle),
        Operation::ProcessRead(request) => runtime
            .processes()
            .read(context, request)
            .await
            .map(OperationResult::ReadExecution),
        Operation::ProcessWrite(request) => runtime
            .processes()
            .write(context, request)
            .await
            .map(OperationResult::ProcessInput),
        Operation::ProcessResize(request) => runtime
            .processes()
            .resize(context, request)
            .await
            .map(|()| OperationResult::Unit),
        Operation::ProcessSignal(request) => runtime
            .processes()
            .signal(context, request)
            .await
            .map(|()| OperationResult::Unit),
        Operation::ProcessTerminate(request) => runtime
            .processes()
            .terminate(context, request)
            .await
            .map(OperationResult::TerminateExecution),
    }
}

/// Validates a request envelope and always returns a correlated response.
pub async fn dispatch_request(
    runtime: &dyn ExecutionRuntime,
    context: &OperationContext,
    request: RequestEnvelope,
) -> ResponseEnvelope {
    let RequestEnvelope {
        version,
        request_id,
        operation,
    } = request;

    if version != PROTOCOL_VERSION {
        return ResponseEnvelope::error(
            request_id,
            ExecutionError::new(
                ExecutionErrorCode::Unsupported,
                format!(
                    "unsupported execution protocol version {version}; expected {PROTOCOL_VERSION}"
                ),
            )
            .with_detail("received_version", u64::from(version))
            .with_detail("supported_version", u64::from(PROTOCOL_VERSION)),
        );
    }

    match dispatch_operation(runtime, context, operation).await {
        Ok(result) => ResponseEnvelope::success(request_id, result),
        Err(error) => ResponseEnvelope::error(request_id, error),
    }
}

#[cfg(test)]
mod tests {
    use async_trait::async_trait;
    use execution_core::{
        BinaryData, CreateDirectoryRequest, ExecutionFeatures, ExecutionHandle,
        ExecutionHostDescriptor, ExecutionHostId, ExecutionId, ExecutionLimits, ExecutionPath,
        ExecutionState, FileKind, FileMetadata, FileRevision, FileSystem, ListDirectoryRequest,
        ListDirectoryResult, OperatingSystem, PathConvention, ProcessRuntime, ReadExecutionRequest,
        ReadExecutionResult, ReadFileRequest, ReadFileResult, RemovePathRequest, RemovePathResult,
        ResizePtyRequest, RootId, SignalExecutionRequest, StartExecutionRequest, StatRequest,
        SupervisorGenerationId, TerminateExecutionRequest, TerminateExecutionResult, TimestampMs,
        WriteFileRequest, WriteFileResult, WriteProcessInputRequest, WriteProcessInputResult,
    };

    use crate::RequestId;

    use super::*;

    struct FakeRuntime {
        descriptor: ExecutionHostDescriptor,
        filesystem: FakeFileSystem,
        processes: FakeProcesses,
    }

    impl FakeRuntime {
        fn new() -> Self {
            let generation = SupervisorGenerationId::generate();
            Self {
                descriptor: ExecutionHostDescriptor {
                    host_id: ExecutionHostId::generate(),
                    supervisor_generation_id: generation.clone(),
                    operating_system: OperatingSystem::Linux,
                    architecture: "x86_64".to_string(),
                    path_convention: PathConvention::Unix,
                    roots: vec![execution_core::ExecutionRoot {
                        id: root_id(),
                        name: "Workspace".to_string(),
                        native_path: "/workspace".to_string(),
                        read_only: false,
                    }],
                    features: ExecutionFeatures {
                        pty: true,
                        process_signals: true,
                        file_revisions: true,
                    },
                    limits: ExecutionLimits::default(),
                },
                filesystem: FakeFileSystem,
                processes: FakeProcesses { generation },
            }
        }
    }

    impl ExecutionRuntime for FakeRuntime {
        fn descriptor(&self) -> &ExecutionHostDescriptor {
            &self.descriptor
        }

        fn filesystem(&self) -> &dyn FileSystem {
            &self.filesystem
        }

        fn processes(&self) -> &dyn ProcessRuntime {
            &self.processes
        }
    }

    struct FakeFileSystem;

    #[async_trait]
    impl FileSystem for FakeFileSystem {
        async fn stat(
            &self,
            _context: &OperationContext,
            request: StatRequest,
        ) -> ExecutionResult<FileMetadata> {
            Ok(metadata(request.path))
        }

        async fn read(
            &self,
            _context: &OperationContext,
            request: ReadFileRequest,
        ) -> ExecutionResult<ReadFileResult> {
            Ok(ReadFileResult {
                metadata: metadata(request.path),
                data: BinaryData::new(b"contents".to_vec()),
                offset: request.offset,
                eof: true,
            })
        }

        async fn write(
            &self,
            _context: &OperationContext,
            request: WriteFileRequest,
        ) -> ExecutionResult<WriteFileResult> {
            Ok(WriteFileResult {
                path: request.path,
                existed: false,
                revision: revision(),
                bytes_written: u64::try_from(request.data.len()).unwrap_or(u64::MAX),
            })
        }

        async fn create_directory(
            &self,
            _context: &OperationContext,
            _request: CreateDirectoryRequest,
        ) -> ExecutionResult<()> {
            Ok(())
        }

        async fn remove(
            &self,
            _context: &OperationContext,
            _request: RemovePathRequest,
        ) -> ExecutionResult<RemovePathResult> {
            Ok(RemovePathResult { removed: true })
        }

        async fn list(
            &self,
            _context: &OperationContext,
            _request: ListDirectoryRequest,
        ) -> ExecutionResult<ListDirectoryResult> {
            Ok(ListDirectoryResult {
                entries: Vec::new(),
                next_cursor: None,
            })
        }
    }

    struct FakeProcesses {
        generation: SupervisorGenerationId,
    }

    #[async_trait]
    impl ProcessRuntime for FakeProcesses {
        async fn start(
            &self,
            _context: &OperationContext,
            request: StartExecutionRequest,
        ) -> ExecutionResult<ExecutionHandle> {
            Ok(ExecutionHandle {
                execution_id: request.execution_id,
                supervisor_generation_id: self.generation.clone(),
                state: ExecutionState::Running,
                started_at: Some(TimestampMs(1)),
            })
        }

        async fn read(
            &self,
            _context: &OperationContext,
            request: ReadExecutionRequest,
        ) -> ExecutionResult<ReadExecutionResult> {
            Ok(ReadExecutionResult {
                events: Vec::new(),
                next_sequence: request.after_sequence,
                state: ExecutionState::Running,
                exit_code: None,
            })
        }

        async fn write(
            &self,
            _context: &OperationContext,
            _request: WriteProcessInputRequest,
        ) -> ExecutionResult<WriteProcessInputResult> {
            Ok(WriteProcessInputResult {
                status: execution_core::ProcessInputStatus::Accepted,
            })
        }

        async fn resize(
            &self,
            _context: &OperationContext,
            _request: ResizePtyRequest,
        ) -> ExecutionResult<()> {
            Ok(())
        }

        async fn signal(
            &self,
            _context: &OperationContext,
            _request: SignalExecutionRequest,
        ) -> ExecutionResult<()> {
            Ok(())
        }

        async fn terminate(
            &self,
            _context: &OperationContext,
            _request: TerminateExecutionRequest,
        ) -> ExecutionResult<TerminateExecutionResult> {
            Ok(TerminateExecutionResult { was_running: true })
        }
    }

    fn root_id() -> RootId {
        RootId::new("workspace").expect("valid root ID")
    }

    fn path() -> ExecutionPath {
        ExecutionPath::new(root_id(), "file.txt").expect("valid path")
    }

    fn revision() -> FileRevision {
        FileRevision::new("revision-1").expect("valid revision")
    }

    fn metadata(path: ExecutionPath) -> FileMetadata {
        FileMetadata {
            path,
            kind: FileKind::File,
            size: 8,
            modified_at: Some(TimestampMs(1)),
            revision: Some(revision()),
        }
    }

    #[tokio::test]
    async fn describe_and_filesystem_operations_are_dispatched() {
        let runtime = FakeRuntime::new();
        let descriptor =
            dispatch_operation(&runtime, &OperationContext::new(), Operation::Describe)
                .await
                .expect("describe runtime");
        assert_eq!(
            descriptor,
            OperationResult::HostDescriptor(runtime.descriptor.clone())
        );

        let result = dispatch_operation(
            &runtime,
            &OperationContext::new(),
            Operation::FilesystemRead(ReadFileRequest {
                path: path(),
                offset: 2,
                max_bytes: 32,
                follow_symlinks: true,
            }),
        )
        .await
        .expect("read file");
        let OperationResult::ReadFile(result) = result else {
            panic!("expected read-file result");
        };
        assert_eq!(result.data.as_slice(), b"contents");
        assert_eq!(result.offset, 2);
    }

    #[tokio::test]
    async fn process_operations_are_dispatched() {
        let runtime = FakeRuntime::new();
        let execution_id = ExecutionId::generate();
        let result = dispatch_operation(
            &runtime,
            &OperationContext::new(),
            Operation::ProcessRead(ReadExecutionRequest {
                execution_id,
                supervisor_generation_id: runtime.descriptor.supervisor_generation_id.clone(),
                after_sequence: 7,
                max_bytes: 1024,
                wait_ms: None,
            }),
        )
        .await
        .expect("read process");
        let OperationResult::ReadExecution(result) = result else {
            panic!("expected read-execution result");
        };
        assert_eq!(result.next_sequence, 7);
    }

    #[tokio::test]
    async fn invalid_operations_return_correlated_error_responses() {
        let runtime = FakeRuntime::new();
        let request_id = RequestId::new("request-1").expect("valid request ID");
        let response = dispatch_request(
            &runtime,
            &OperationContext::new(),
            RequestEnvelope::new(
                request_id.clone(),
                Operation::FilesystemRead(ReadFileRequest {
                    path: path(),
                    offset: 0,
                    max_bytes: 0,
                    follow_symlinks: true,
                }),
            ),
        )
        .await;
        let ResponseEnvelope::Error {
            request_id: returned_id,
            error,
            ..
        } = response
        else {
            panic!("expected error response");
        };
        assert_eq!(returned_id, request_id);
        assert_eq!(error.code, ExecutionErrorCode::InvalidRequest);
    }

    #[tokio::test]
    async fn unsupported_versions_return_correlated_errors() {
        let runtime = FakeRuntime::new();
        let request_id = RequestId::new("request-1").expect("valid request ID");
        let response = dispatch_request(
            &runtime,
            &OperationContext::new(),
            RequestEnvelope {
                version: PROTOCOL_VERSION + 1,
                request_id: request_id.clone(),
                operation: Operation::Describe,
            },
        )
        .await;
        let ResponseEnvelope::Error {
            request_id: returned_id,
            error,
            ..
        } = response
        else {
            panic!("expected error response");
        };
        assert_eq!(returned_id, request_id);
        assert_eq!(error.code, ExecutionErrorCode::Unsupported);
    }

    #[tokio::test]
    async fn cancelled_context_stops_dispatch_before_runtime_work() {
        let runtime = FakeRuntime::new();
        let context = OperationContext::new();
        context.cancel();
        let error = dispatch_operation(&runtime, &context, Operation::Describe)
            .await
            .expect_err("cancelled request must fail");
        assert_eq!(error.code, ExecutionErrorCode::Cancelled);
    }
}
