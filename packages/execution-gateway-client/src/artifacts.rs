use async_trait::async_trait;
use execution_contracts::{ArtifactMetadata, GetArtifactMetadataRequest, OpenArtifactRequest};
use execution_protocol::Operation;
use execution_runtime::{ArtifactChunkStream, ArtifactStore, ExecutionResult, OperationContext};

use crate::GatewayExecutionEnvironment;

#[async_trait]
impl ArtifactStore for GatewayExecutionEnvironment {
    async fn metadata(
        &self,
        context: &OperationContext,
        request: GetArtifactMetadataRequest,
    ) -> ExecutionResult<ArtifactMetadata> {
        self.execute(context, Operation::ArtifactMetadata(request))
            .await
    }

    async fn open(
        &self,
        context: &OperationContext,
        request: OpenArtifactRequest,
    ) -> ExecutionResult<ArtifactChunkStream> {
        self.stream(context, Operation::ArtifactOpen(request)).await
    }
}
