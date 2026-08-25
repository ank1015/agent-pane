use async_trait::async_trait;
use execution_contracts::{
    CopyPathRequest, CreateDirectoryRequest, FileMetadata, InspectRequest, ListRequest, ListResult,
    MovePathRequest, ReadBytesRequest, ReadBytesResult, RemovePathRequest, WalkRequest, WalkResult,
    WriteBytesRequest, WriteBytesResult,
};
use execution_protocol::Operation;
use execution_runtime::{BasicFileSystem, ExecutionResult, OperationContext};

use crate::GatewayMachineRuntime;

#[async_trait]
impl BasicFileSystem for GatewayMachineRuntime {
    async fn inspect(
        &self,
        context: &OperationContext,
        request: InspectRequest,
    ) -> ExecutionResult<FileMetadata> {
        self.execute(context, Operation::FilesystemInspect(request))
            .await
    }

    async fn read_bytes(
        &self,
        context: &OperationContext,
        request: ReadBytesRequest,
    ) -> ExecutionResult<ReadBytesResult> {
        self.execute(context, Operation::FilesystemReadBytes(request))
            .await
    }

    async fn write_bytes(
        &self,
        context: &OperationContext,
        request: WriteBytesRequest,
    ) -> ExecutionResult<WriteBytesResult> {
        self.execute(context, Operation::FilesystemWriteBytes(request))
            .await
    }

    async fn create_directory(
        &self,
        context: &OperationContext,
        request: CreateDirectoryRequest,
    ) -> ExecutionResult<()> {
        self.execute(context, Operation::FilesystemCreateDirectory(request))
            .await
    }

    async fn remove(
        &self,
        context: &OperationContext,
        request: RemovePathRequest,
    ) -> ExecutionResult<()> {
        self.execute(context, Operation::FilesystemRemove(request))
            .await
    }

    async fn move_path(
        &self,
        context: &OperationContext,
        request: MovePathRequest,
    ) -> ExecutionResult<()> {
        self.execute(context, Operation::FilesystemMove(request))
            .await
    }

    async fn copy_path(
        &self,
        context: &OperationContext,
        request: CopyPathRequest,
    ) -> ExecutionResult<()> {
        self.execute(context, Operation::FilesystemCopy(request))
            .await
    }

    async fn list_raw(
        &self,
        context: &OperationContext,
        request: ListRequest,
    ) -> ExecutionResult<ListResult> {
        self.execute(context, Operation::FilesystemListRaw(request))
            .await
    }

    async fn walk(
        &self,
        context: &OperationContext,
        request: WalkRequest,
    ) -> ExecutionResult<WalkResult> {
        self.execute(context, Operation::FilesystemWalk(request))
            .await
    }
}
