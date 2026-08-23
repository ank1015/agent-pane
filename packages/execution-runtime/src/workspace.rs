use async_trait::async_trait;
use execution_contracts::{
    AbortMutationRequest, ApplyMutationRequest, CommitMutationRequest, FileMetadata,
    InspectManyRequest, InspectManyResult, InspectRequest, ListRequest, ListResult, MutationResult,
    PrepareMutationRequest, PreparedMutation, ReadRequest, ReadResult, SearchRequest, SearchResult,
};

use crate::{ExecutionResult, OperationContext};

/// Efficient machine-side workspace reads, listings, and search.
#[async_trait]
pub trait WorkspaceQuery: Send + Sync {
    async fn inspect(
        &self,
        context: &OperationContext,
        request: InspectRequest,
    ) -> ExecutionResult<FileMetadata>;

    async fn inspect_many(
        &self,
        context: &OperationContext,
        request: InspectManyRequest,
    ) -> ExecutionResult<InspectManyResult>;

    async fn read(
        &self,
        context: &OperationContext,
        request: ReadRequest,
    ) -> ExecutionResult<ReadResult>;

    async fn list(
        &self,
        context: &OperationContext,
        request: ListRequest,
    ) -> ExecutionResult<ListResult>;

    async fn search(
        &self,
        context: &OperationContext,
        request: SearchRequest,
    ) -> ExecutionResult<SearchResult>;
}

/// Validated machine-side mutations, including approval-friendly staging.
#[async_trait]
pub trait WorkspaceMutation: Send + Sync {
    async fn prepare(
        &self,
        context: &OperationContext,
        request: PrepareMutationRequest,
    ) -> ExecutionResult<PreparedMutation>;

    async fn commit(
        &self,
        context: &OperationContext,
        request: CommitMutationRequest,
    ) -> ExecutionResult<MutationResult>;

    async fn abort(
        &self,
        context: &OperationContext,
        request: AbortMutationRequest,
    ) -> ExecutionResult<()>;

    /// Validates and commits a pre-approved plan in one logical operation.
    async fn apply(
        &self,
        context: &OperationContext,
        request: ApplyMutationRequest,
    ) -> ExecutionResult<MutationResult>;
}
