use std::pin::Pin;

use async_trait::async_trait;
use execution_contracts::{
    ArtifactChunk, ArtifactMetadata, GetArtifactMetadataRequest, OpenArtifactRequest,
};
use futures_core::Stream;

use crate::{ExecutionResult, OperationContext};

/// Bounded artifact chunks. A backend may map these to binary transport frames.
pub type ArtifactChunkStream =
    Pin<Box<dyn Stream<Item = ExecutionResult<ArtifactChunk>> + Send + 'static>>;

/// Storage for large files, media, mutation previews, and complete process output.
#[async_trait]
pub trait ArtifactStore: Send + Sync {
    async fn metadata(
        &self,
        context: &OperationContext,
        request: GetArtifactMetadataRequest,
    ) -> ExecutionResult<ArtifactMetadata>;

    async fn open(
        &self,
        context: &OperationContext,
        request: OpenArtifactRequest,
    ) -> ExecutionResult<ArtifactChunkStream>;
}
