use async_trait::async_trait;
use execution_core::{
    CreateDirectoryRequest, ExecutionHandle, ExecutionHostDescriptor, ExecutionResult,
    ExecutionRuntime, FileMetadata, FileSystem, ListDirectoryRequest, ListDirectoryResult,
    OperationContext, ProcessRuntime, ReadExecutionRequest, ReadExecutionResult, ReadFileRequest,
    ReadFileResult, RemovePathRequest, RemovePathResult, ResizePtyRequest, SignalExecutionRequest,
    StartExecutionRequest, StatRequest, TerminateExecutionRequest, TerminateExecutionResult,
    WriteFileRequest, WriteFileResult, WriteProcessInputRequest, WriteProcessInputResult,
};
use execution_wire::{Operation, OperationResult};
use url::Url;

use crate::ExecutionClient;

/// One selected host, usable through the execution-core runtime traits.
///
/// Clones share the HTTP connection pool, not process cursors or mutable tool
/// state. The descriptor is a snapshot from connection time. Process requests
/// must retain the generation returned in their original execution handle;
/// this client never substitutes the descriptor's generation into a request.
/// Dropping this value does not terminate remote processes.
#[derive(Clone, Debug)]
pub struct GatewayHostRuntime {
    pub(crate) client: ExecutionClient,
    pub(crate) url: Url,
    pub(crate) descriptor: ExecutionHostDescriptor,
}

impl GatewayHostRuntime {
    async fn call(
        &self,
        context: &OperationContext,
        operation: Operation,
    ) -> ExecutionResult<OperationResult> {
        self.client.call(context, &self.url, operation).await
    }
}

impl ExecutionRuntime for GatewayHostRuntime {
    fn descriptor(&self) -> &ExecutionHostDescriptor {
        &self.descriptor
    }

    fn filesystem(&self) -> &dyn FileSystem {
        self
    }

    fn processes(&self) -> &dyn ProcessRuntime {
        self
    }
}

#[async_trait]
impl FileSystem for GatewayHostRuntime {
    async fn stat(
        &self,
        context: &OperationContext,
        request: StatRequest,
    ) -> ExecutionResult<FileMetadata> {
        expect_result(
            "filesystem.stat",
            self.call(context, Operation::FilesystemStat(request))
                .await?,
            |result| match result {
                OperationResult::FileMetadata(value) => Some(value),
                _ => None,
            },
        )
    }

    async fn read(
        &self,
        context: &OperationContext,
        request: ReadFileRequest,
    ) -> ExecutionResult<ReadFileResult> {
        expect_result(
            "filesystem.read",
            self.call(context, Operation::FilesystemRead(request))
                .await?,
            |result| match result {
                OperationResult::ReadFile(value) => Some(value),
                _ => None,
            },
        )
    }

    async fn write(
        &self,
        context: &OperationContext,
        request: WriteFileRequest,
    ) -> ExecutionResult<WriteFileResult> {
        expect_result(
            "filesystem.write",
            self.call(context, Operation::FilesystemWrite(request))
                .await?,
            |result| match result {
                OperationResult::WriteFile(value) => Some(value),
                _ => None,
            },
        )
    }

    async fn create_directory(
        &self,
        context: &OperationContext,
        request: CreateDirectoryRequest,
    ) -> ExecutionResult<()> {
        expect_unit(
            "filesystem.create_directory",
            self.call(context, Operation::FilesystemCreateDirectory(request))
                .await?,
        )
    }

    async fn remove(
        &self,
        context: &OperationContext,
        request: RemovePathRequest,
    ) -> ExecutionResult<RemovePathResult> {
        expect_result(
            "filesystem.remove",
            self.call(context, Operation::FilesystemRemove(request))
                .await?,
            |result| match result {
                OperationResult::RemovePath(value) => Some(value),
                _ => None,
            },
        )
    }

    async fn list(
        &self,
        context: &OperationContext,
        request: ListDirectoryRequest,
    ) -> ExecutionResult<ListDirectoryResult> {
        expect_result(
            "filesystem.list",
            self.call(context, Operation::FilesystemList(request))
                .await?,
            |result| match result {
                OperationResult::ListDirectory(value) => Some(value),
                _ => None,
            },
        )
    }
}

#[async_trait]
impl ProcessRuntime for GatewayHostRuntime {
    async fn start(
        &self,
        context: &OperationContext,
        request: StartExecutionRequest,
    ) -> ExecutionResult<ExecutionHandle> {
        expect_result(
            "process.start",
            self.call(context, Operation::ProcessStart(request)).await?,
            |result| match result {
                OperationResult::ExecutionHandle(value) => Some(value),
                _ => None,
            },
        )
    }

    async fn read(
        &self,
        context: &OperationContext,
        request: ReadExecutionRequest,
    ) -> ExecutionResult<ReadExecutionResult> {
        expect_result(
            "process.read",
            self.call(context, Operation::ProcessRead(request)).await?,
            |result| match result {
                OperationResult::ReadExecution(value) => Some(value),
                _ => None,
            },
        )
    }

    async fn write(
        &self,
        context: &OperationContext,
        request: WriteProcessInputRequest,
    ) -> ExecutionResult<WriteProcessInputResult> {
        expect_result(
            "process.write",
            self.call(context, Operation::ProcessWrite(request)).await?,
            |result| match result {
                OperationResult::ProcessInput(value) => Some(value),
                _ => None,
            },
        )
    }

    async fn resize(
        &self,
        context: &OperationContext,
        request: ResizePtyRequest,
    ) -> ExecutionResult<()> {
        expect_unit(
            "process.resize",
            self.call(context, Operation::ProcessResize(request))
                .await?,
        )
    }

    async fn signal(
        &self,
        context: &OperationContext,
        request: SignalExecutionRequest,
    ) -> ExecutionResult<()> {
        expect_unit(
            "process.signal",
            self.call(context, Operation::ProcessSignal(request))
                .await?,
        )
    }

    async fn terminate(
        &self,
        context: &OperationContext,
        request: TerminateExecutionRequest,
    ) -> ExecutionResult<TerminateExecutionResult> {
        expect_result(
            "process.terminate",
            self.call(context, Operation::ProcessTerminate(request))
                .await?,
            |result| match result {
                OperationResult::TerminateExecution(value) => Some(value),
                _ => None,
            },
        )
    }
}

fn expect_result<T>(
    operation: &str,
    result: OperationResult,
    extract: impl FnOnce(OperationResult) -> Option<T>,
) -> ExecutionResult<T> {
    extract(result).ok_or_else(|| {
        crate::error::protocol_error(format!(
            "gateway returned an unexpected result for {operation}"
        ))
    })
}

fn expect_unit(operation: &str, result: OperationResult) -> ExecutionResult<()> {
    if result == OperationResult::Unit {
        Ok(())
    } else {
        Err(crate::error::protocol_error(format!(
            "gateway returned an unexpected result for {operation}"
        )))
    }
}
