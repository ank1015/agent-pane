use std::{collections::HashMap, path::PathBuf};

use async_trait::async_trait;
use base64::{Engine, engine::general_purpose::STANDARD};
use execution_contracts::{
    ArtifactChunk, ArtifactId, ArtifactKind, ArtifactMetadata, Base64Data, ExecutionErrorCode,
    GetArtifactMetadataRequest, OpenArtifactRequest, TimestampMs, Validate,
};
use execution_runtime::{ArtifactChunkStream, ArtifactStore, ExecutionResult, OperationContext};
use futures_util::stream;
use sha2::{Digest, Sha256};
use tokio::{
    io::{AsyncReadExt, AsyncSeekExt},
    sync::RwLock,
};
use uuid::Uuid;

use crate::error::{LocalExecutionError, error, io_error};

pub(crate) struct LocalArtifactStore {
    directory: PathBuf,
    metadata: RwLock<HashMap<ArtifactId, ArtifactMetadata>>,
}

impl LocalArtifactStore {
    pub async fn new(directory: PathBuf) -> Result<Self, LocalExecutionError> {
        tokio::fs::create_dir_all(&directory).await?;
        Ok(Self {
            directory,
            metadata: RwLock::new(HashMap::new()),
        })
    }

    pub async fn put(
        &self,
        kind: ArtifactKind,
        name: Option<String>,
        mime_type: Option<String>,
        bytes: &[u8],
    ) -> ExecutionResult<ArtifactMetadata> {
        let artifact_id =
            ArtifactId::new(Uuid::now_v7().to_string()).expect("UUID artifact identifier is valid");
        let path = self.path(&artifact_id);
        tokio::fs::write(&path, bytes)
            .await
            .map_err(|source| io_error(&path, source))?;
        let metadata = ArtifactMetadata {
            artifact_id: artifact_id.clone(),
            kind,
            size: bytes.len() as u64,
            created_at: now(),
            name,
            mime_type,
            expires_at: None,
            checksum: Some(hex_digest(bytes)),
        };
        self.metadata
            .write()
            .await
            .insert(artifact_id, metadata.clone());
        Ok(metadata)
    }

    pub async fn read_all(&self, artifact_id: &ArtifactId) -> ExecutionResult<Vec<u8>> {
        self.lookup(artifact_id).await?;
        let path = self.path(artifact_id);
        tokio::fs::read(&path)
            .await
            .map_err(|source| io_error(&path, source))
    }

    async fn lookup(&self, artifact_id: &ArtifactId) -> ExecutionResult<ArtifactMetadata> {
        self.metadata
            .read()
            .await
            .get(artifact_id)
            .cloned()
            .ok_or_else(|| {
                error(
                    ExecutionErrorCode::NotFound,
                    format!("artifact `{artifact_id}` was not found"),
                )
            })
    }

    fn path(&self, artifact_id: &ArtifactId) -> PathBuf {
        self.directory.join(artifact_id.as_str())
    }
}

#[async_trait]
impl ArtifactStore for LocalArtifactStore {
    async fn metadata(
        &self,
        _context: &OperationContext,
        request: GetArtifactMetadataRequest,
    ) -> ExecutionResult<ArtifactMetadata> {
        self.lookup(&request.artifact_id).await
    }

    async fn open(
        &self,
        _context: &OperationContext,
        request: OpenArtifactRequest,
    ) -> ExecutionResult<ArtifactChunkStream> {
        request.validate().map_err(|source| {
            error(
                ExecutionErrorCode::InvalidRequest,
                format!("invalid artifact request: {source}"),
            )
        })?;
        let metadata = self.lookup(&request.artifact_id).await?;
        if request.offset > metadata.size {
            return Err(error(
                ExecutionErrorCode::InvalidRequest,
                "artifact offset exceeds artifact size",
            ));
        }
        let path = self.path(&request.artifact_id);
        let mut file = tokio::fs::File::open(&path)
            .await
            .map_err(|source| io_error(&path, source))?;
        file.seek(std::io::SeekFrom::Start(request.offset))
            .await
            .map_err(|source| io_error(&path, source))?;
        let chunk_size = usize::try_from(request.max_bytes.min(1024 * 1024))
            .unwrap_or(1024 * 1024)
            .max(1);
        let state = ArtifactStreamState {
            file,
            artifact_id: request.artifact_id,
            offset: request.offset,
            size: metadata.size,
            chunk_size,
            path,
            complete: false,
        };
        Ok(Box::pin(stream::unfold(state, |mut state| async move {
            if state.complete {
                return None;
            }
            let mut bytes = vec![0; state.chunk_size];
            match state.file.read(&mut bytes).await {
                Ok(read) => {
                    bytes.truncate(read);
                    let offset = state.offset;
                    state.offset += read as u64;
                    let eof = state.offset >= state.size || read == 0;
                    state.complete = eof;
                    let chunk = ArtifactChunk {
                        artifact_id: state.artifact_id.clone(),
                        offset,
                        data: Base64Data(STANDARD.encode(bytes)),
                        eof,
                    };
                    Some((Ok(chunk), state))
                }
                Err(source) => {
                    state.complete = true;
                    let path = state.path.clone();
                    Some((Err(io_error(&path, source)), state))
                }
            }
        })))
    }
}

struct ArtifactStreamState {
    file: tokio::fs::File,
    artifact_id: ArtifactId,
    offset: u64,
    size: u64,
    chunk_size: usize,
    path: PathBuf,
    complete: bool,
}

fn now() -> TimestampMs {
    let milliseconds = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis();
    TimestampMs(u64::try_from(milliseconds).unwrap_or(u64::MAX))
}

pub(crate) fn hex_digest(bytes: &[u8]) -> String {
    format!("{:x}", Sha256::digest(bytes))
}
