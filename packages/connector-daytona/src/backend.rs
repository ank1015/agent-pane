use std::sync::Arc;

use async_trait::async_trait;
use execution_contracts::{
    AbortMutationRequest, ApplyMutationRequest, CommitMutationRequest, CopyPathRequest,
    CreateDirectoryRequest, ExecutionErrorCode, FileMetadata, InspectManyRequest,
    InspectManyResult, InspectRequest, ListRequest, ListResult, MovePathRequest, MutationResult,
    PrepareMutationRequest, PreparedMutation, ReadBytesRequest, ReadBytesResult, ReadRequest,
    ReadResult, RemovePathRequest, SearchRequest, SearchResult, Validate, WalkRequest, WalkResult,
    WriteBytesRequest, WriteBytesResult,
};
use execution_runtime::{
    BasicFileSystem, ExecutionResult, OperationContext, WorkspaceMutation, WorkspaceQuery,
};

use crate::{
    error::{execution_error, invalid_request},
    runner::InlineRunner,
};

#[derive(Clone)]
pub(crate) struct DaytonaBackend {
    pub(crate) runner: Arc<InlineRunner>,
}

impl DaytonaBackend {
    pub fn new(runner: Arc<InlineRunner>) -> Self {
        Self { runner }
    }

    fn check(context: &OperationContext) -> ExecutionResult<()> {
        if context.is_cancelled() {
            return Err(execution_error(
                ExecutionErrorCode::Cancelled,
                "operation was cancelled",
            ));
        }
        if context
            .remaining()
            .is_some_and(|remaining| remaining.is_zero())
        {
            return Err(execution_error(
                ExecutionErrorCode::DeadlineExceeded,
                "operation deadline elapsed",
            ));
        }
        Ok(())
    }
}

#[async_trait]
impl WorkspaceQuery for DaytonaBackend {
    async fn inspect(
        &self,
        context: &OperationContext,
        request: InspectRequest,
    ) -> ExecutionResult<FileMetadata> {
        Self::check(context)?;
        request.path.validate().map_err(invalid_request)?;
        self.runner.call("inspect", &request).await
    }

    async fn inspect_many(
        &self,
        context: &OperationContext,
        request: InspectManyRequest,
    ) -> ExecutionResult<InspectManyResult> {
        Self::check(context)?;
        request.validate().map_err(invalid_request)?;
        self.runner.call("inspect_many", &request).await
    }

    async fn read(
        &self,
        context: &OperationContext,
        request: ReadRequest,
    ) -> ExecutionResult<ReadResult> {
        Self::check(context)?;
        request.validate().map_err(invalid_request)?;
        self.runner.call("read", &request).await
    }

    async fn list(
        &self,
        context: &OperationContext,
        request: ListRequest,
    ) -> ExecutionResult<ListResult> {
        Self::check(context)?;
        request.validate().map_err(invalid_request)?;
        self.runner.call("list", &request).await
    }

    async fn search(
        &self,
        context: &OperationContext,
        request: SearchRequest,
    ) -> ExecutionResult<SearchResult> {
        Self::check(context)?;
        request.validate().map_err(invalid_request)?;
        self.runner.call("search", &request).await
    }
}

#[async_trait]
impl BasicFileSystem for DaytonaBackend {
    async fn inspect(
        &self,
        context: &OperationContext,
        request: InspectRequest,
    ) -> ExecutionResult<FileMetadata> {
        WorkspaceQuery::inspect(self, context, request).await
    }

    async fn read_bytes(
        &self,
        context: &OperationContext,
        request: ReadBytesRequest,
    ) -> ExecutionResult<ReadBytesResult> {
        Self::check(context)?;
        request.validate().map_err(invalid_request)?;
        self.runner.call("read_bytes", &request).await
    }

    async fn write_bytes(
        &self,
        context: &OperationContext,
        request: WriteBytesRequest,
    ) -> ExecutionResult<WriteBytesResult> {
        Self::check(context)?;
        request.path.validate().map_err(invalid_request)?;
        self.runner.call("write_bytes", &request).await
    }

    async fn create_directory(
        &self,
        context: &OperationContext,
        request: CreateDirectoryRequest,
    ) -> ExecutionResult<()> {
        Self::check(context)?;
        request.path.validate().map_err(invalid_request)?;
        self.runner.call("create_directory", &request).await
    }

    async fn remove(
        &self,
        context: &OperationContext,
        request: RemovePathRequest,
    ) -> ExecutionResult<()> {
        Self::check(context)?;
        request.path.validate().map_err(invalid_request)?;
        self.runner.call("remove", &request).await
    }

    async fn move_path(
        &self,
        context: &OperationContext,
        request: MovePathRequest,
    ) -> ExecutionResult<()> {
        Self::check(context)?;
        request.source.validate().map_err(invalid_request)?;
        request.destination.validate().map_err(invalid_request)?;
        self.runner.call("move_path", &request).await
    }

    async fn copy_path(
        &self,
        context: &OperationContext,
        request: CopyPathRequest,
    ) -> ExecutionResult<()> {
        Self::check(context)?;
        request.source.validate().map_err(invalid_request)?;
        request.destination.validate().map_err(invalid_request)?;
        self.runner.call("copy_path", &request).await
    }

    async fn list_raw(
        &self,
        context: &OperationContext,
        request: ListRequest,
    ) -> ExecutionResult<ListResult> {
        WorkspaceQuery::list(self, context, request).await
    }

    async fn walk(
        &self,
        context: &OperationContext,
        request: WalkRequest,
    ) -> ExecutionResult<WalkResult> {
        Self::check(context)?;
        request.validate().map_err(invalid_request)?;
        self.runner.call("walk", &request).await
    }
}

#[async_trait]
impl WorkspaceMutation for DaytonaBackend {
    async fn prepare(
        &self,
        context: &OperationContext,
        request: PrepareMutationRequest,
    ) -> ExecutionResult<PreparedMutation> {
        Self::check(context)?;
        request.validate().map_err(invalid_request)?;
        self.runner.call("prepare_mutation", &request).await
    }

    async fn commit(
        &self,
        context: &OperationContext,
        request: CommitMutationRequest,
    ) -> ExecutionResult<MutationResult> {
        Self::check(context)?;
        self.runner.call("commit_mutation", &request).await
    }

    async fn abort(
        &self,
        context: &OperationContext,
        request: AbortMutationRequest,
    ) -> ExecutionResult<()> {
        Self::check(context)?;
        self.runner.call("abort_mutation", &request).await
    }

    async fn apply(
        &self,
        context: &OperationContext,
        request: ApplyMutationRequest,
    ) -> ExecutionResult<MutationResult> {
        Self::check(context)?;
        request.validate().map_err(invalid_request)?;
        self.runner.call("apply_mutation", &request).await
    }
}
