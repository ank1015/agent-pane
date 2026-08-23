use std::sync::Arc;

use async_trait::async_trait;
use execution_contracts::{
    ArtifactChunk, ArtifactMetadata, GetArtifactMetadataRequest, OpenArtifactRequest, Validate,
};
use execution_runtime::{ArtifactChunkStream, ArtifactStore, ExecutionResult, OperationContext};
use futures_util::stream;

use crate::{error::invalid_request, runner::InlineRunner};

pub(crate) struct TensorlakeArtifactStore {
    runner: Arc<InlineRunner>,
}

impl TensorlakeArtifactStore {
    pub fn new(runner: Arc<InlineRunner>) -> Self {
        Self { runner }
    }
}

#[async_trait]
impl ArtifactStore for TensorlakeArtifactStore {
    async fn metadata(
        &self,
        _context: &OperationContext,
        request: GetArtifactMetadataRequest,
    ) -> ExecutionResult<ArtifactMetadata> {
        self.runner.call("artifact_metadata", &request).await
    }

    async fn open(
        &self,
        _context: &OperationContext,
        request: OpenArtifactRequest,
    ) -> ExecutionResult<ArtifactChunkStream> {
        request.validate().map_err(invalid_request)?;
        let runner = Arc::clone(&self.runner);
        let state = Some((runner, request));
        Ok(Box::pin(stream::unfold(state, |state| async move {
            let (runner, request) = state?;
            match runner
                .call::<_, ArtifactChunk>("artifact_open", &request)
                .await
            {
                Ok(chunk) => {
                    let next = (!chunk.eof).then(|| {
                        let next_request = OpenArtifactRequest {
                            artifact_id: request.artifact_id,
                            offset: chunk.offset + decoded_len(&chunk.data.0) as u64,
                            max_bytes: request.max_bytes,
                        };
                        (runner, next_request)
                    });
                    Some((Ok(chunk), next))
                }
                Err(error) => Some((Err(error), None)),
            }
        })))
    }
}

fn decoded_len(base64: &str) -> usize {
    let padding = base64
        .as_bytes()
        .iter()
        .rev()
        .take_while(|byte| **byte == b'=')
        .count();
    base64.len().saturating_mul(3) / 4 - padding
}
