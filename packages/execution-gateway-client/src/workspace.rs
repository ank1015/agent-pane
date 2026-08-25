use async_trait::async_trait;
use execution_contracts::{
    AbortMutationRequest, ApplyMutationRequest, CommitMutationRequest, FileMetadata,
    InspectManyRequest, InspectManyResult, InspectRequest, ListRequest, ListResult, MutationResult,
    PrepareMutationRequest, PreparedMutation, ReadRequest, ReadResult, SearchRequest, SearchResult,
};
use execution_protocol::Operation;
use execution_runtime::{ExecutionResult, OperationContext, WorkspaceMutation, WorkspaceQuery};

use crate::GatewayMachineRuntime;

#[async_trait]
impl WorkspaceQuery for GatewayMachineRuntime {
    async fn inspect(
        &self,
        context: &OperationContext,
        request: InspectRequest,
    ) -> ExecutionResult<FileMetadata> {
        self.execute(context, Operation::WorkspaceInspect(request))
            .await
    }

    async fn inspect_many(
        &self,
        context: &OperationContext,
        request: InspectManyRequest,
    ) -> ExecutionResult<InspectManyResult> {
        self.execute(context, Operation::WorkspaceInspectMany(request))
            .await
    }

    async fn read(
        &self,
        context: &OperationContext,
        request: ReadRequest,
    ) -> ExecutionResult<ReadResult> {
        self.execute(context, Operation::WorkspaceRead(request))
            .await
    }

    async fn list(
        &self,
        context: &OperationContext,
        request: ListRequest,
    ) -> ExecutionResult<ListResult> {
        self.execute(context, Operation::WorkspaceList(request))
            .await
    }

    async fn search(
        &self,
        context: &OperationContext,
        request: SearchRequest,
    ) -> ExecutionResult<SearchResult> {
        self.execute(context, Operation::WorkspaceSearch(request))
            .await
    }
}

#[async_trait]
impl WorkspaceMutation for GatewayMachineRuntime {
    async fn prepare(
        &self,
        context: &OperationContext,
        request: PrepareMutationRequest,
    ) -> ExecutionResult<PreparedMutation> {
        self.execute(context, Operation::MutationPrepare(request))
            .await
    }

    async fn commit(
        &self,
        context: &OperationContext,
        request: CommitMutationRequest,
    ) -> ExecutionResult<MutationResult> {
        self.execute(context, Operation::MutationCommit(request))
            .await
    }

    async fn abort(
        &self,
        context: &OperationContext,
        request: AbortMutationRequest,
    ) -> ExecutionResult<()> {
        self.execute(context, Operation::MutationAbort(request))
            .await
    }

    async fn apply(
        &self,
        context: &OperationContext,
        request: ApplyMutationRequest,
    ) -> ExecutionResult<MutationResult> {
        self.execute(context, Operation::MutationApply(request))
            .await
    }
}
