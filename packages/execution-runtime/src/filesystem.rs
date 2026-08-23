use async_trait::async_trait;
use execution_contracts::{
    CopyPathRequest, CreateDirectoryRequest, FileMetadata, InspectRequest, ListRequest, ListResult,
    MovePathRequest, ReadBytesRequest, ReadBytesResult, RemovePathRequest, WalkRequest, WalkResult,
    WriteBytesRequest, WriteBytesResult,
};

use crate::{ExecutionResult, OperationContext};

/// Primitive compatibility layer for custom tools and constrained providers.
///
/// Normal agent read, search, edit, and patch tools should use the higher-level
/// workspace capabilities instead.
#[async_trait]
pub trait BasicFileSystem: Send + Sync {
    async fn inspect(
        &self,
        context: &OperationContext,
        request: InspectRequest,
    ) -> ExecutionResult<FileMetadata>;

    async fn read_bytes(
        &self,
        context: &OperationContext,
        request: ReadBytesRequest,
    ) -> ExecutionResult<ReadBytesResult>;

    async fn write_bytes(
        &self,
        context: &OperationContext,
        request: WriteBytesRequest,
    ) -> ExecutionResult<WriteBytesResult>;

    async fn create_directory(
        &self,
        context: &OperationContext,
        request: CreateDirectoryRequest,
    ) -> ExecutionResult<()>;

    async fn remove(
        &self,
        context: &OperationContext,
        request: RemovePathRequest,
    ) -> ExecutionResult<()>;

    async fn move_path(
        &self,
        context: &OperationContext,
        request: MovePathRequest,
    ) -> ExecutionResult<()>;

    async fn copy_path(
        &self,
        context: &OperationContext,
        request: CopyPathRequest,
    ) -> ExecutionResult<()>;

    async fn list_raw(
        &self,
        context: &OperationContext,
        request: ListRequest,
    ) -> ExecutionResult<ListResult>;

    async fn walk(
        &self,
        context: &OperationContext,
        request: WalkRequest,
    ) -> ExecutionResult<WalkResult>;
}
