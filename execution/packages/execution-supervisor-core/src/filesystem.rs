use std::{
    collections::HashMap,
    path::{Path, PathBuf},
    sync::Arc,
    time::UNIX_EPOCH,
};

use async_trait::async_trait;
use execution_core::{
    BinaryData, CreateDirectoryRequest, DirectoryCursor, DirectoryEntry, ExecutionErrorCode,
    ExecutionLimits, ExecutionPath, ExecutionResult, FileKind, FileMetadata, FileRevision,
    FileSystem, ListDirectoryRequest, ListDirectoryResult, OperationContext, OperationId,
    ReadFileRequest, ReadFileResult, RemovePathRequest, RemovePathResult, StatRequest, TimestampMs,
    Validate, WriteCondition, WriteFileRequest, WriteFileResult,
};
use serde::Serialize;
use sha2::{Digest, Sha256};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    sync::Mutex,
};
use uuid::Uuid;

use crate::{
    config::SupervisorLimits,
    error::{error, invalid_request, io_error, operation_conflict},
    path_resolver::PathResolver,
};

#[derive(Clone)]
pub(crate) struct SupervisorFileSystem {
    resolver: Arc<PathResolver>,
    limits: SupervisorLimits,
    mutation_state: Arc<Mutex<MutationState>>,
}

#[derive(Default)]
struct MutationState {
    records: HashMap<OperationId, MutationRecord>,
}

#[derive(Clone)]
struct MutationRecord {
    fingerprint: String,
    result: MutationResult,
}

#[derive(Clone)]
enum MutationResult {
    Write(WriteFileResult),
    CreateDirectory,
    Remove(RemovePathResult),
}

impl SupervisorFileSystem {
    pub fn new(resolver: Arc<PathResolver>, limits: SupervisorLimits) -> Self {
        Self {
            resolver,
            limits,
            mutation_state: Arc::new(Mutex::new(MutationState::default())),
        }
    }

    pub fn descriptor_limits(&self) -> ExecutionLimits {
        ExecutionLimits {
            max_read_bytes: Some(self.limits.max_read_bytes),
            max_write_bytes: Some(self.limits.max_write_bytes),
            max_process_read_bytes: Some(self.limits.max_process_read_bytes),
            max_process_input_bytes: Some(self.limits.max_process_input_bytes),
            max_concurrent_processes: Some(self.limits.max_concurrent_processes),
        }
    }

    async fn metadata_for(
        &self,
        spec: &ExecutionPath,
        native_path: &Path,
        follow_symlinks: bool,
    ) -> ExecutionResult<FileMetadata> {
        let metadata = if follow_symlinks {
            tokio::fs::metadata(native_path).await
        } else {
            tokio::fs::symlink_metadata(native_path).await
        }
        .map_err(|source| io_error(native_path, source))?;

        let kind = file_kind(&metadata);
        let revision = match kind {
            FileKind::File => Some(hash_file(native_path).await?),
            FileKind::Symlink => {
                let target = tokio::fs::read_link(native_path)
                    .await
                    .map_err(|source| io_error(native_path, source))?;
                Some(hash_bytes(target.to_string_lossy().as_bytes()))
            }
            FileKind::Directory | FileKind::Other => None,
        };
        Ok(FileMetadata {
            path: spec.clone(),
            kind,
            size: metadata.len(),
            modified_at: modified_at(&metadata),
            revision,
        })
    }

    async fn write_inner(&self, request: &WriteFileRequest) -> ExecutionResult<WriteFileResult> {
        let byte_count = u64::try_from(request.data.len()).unwrap_or(u64::MAX);
        if byte_count > self.limits.max_write_bytes {
            return Err(error(
                ExecutionErrorCode::ResourceExhausted,
                format!(
                    "write contains {byte_count} bytes but the host limit is {}",
                    self.limits.max_write_bytes
                ),
            ));
        }

        let resolved = self
            .resolver
            .resolve_for_write(&request.path, request.follow_symlinks)
            .await?;
        let existing = match tokio::fs::symlink_metadata(&resolved.path).await {
            Ok(metadata) => Some(metadata),
            Err(source) if source.kind() == std::io::ErrorKind::NotFound => None,
            Err(source) => return Err(io_error(&resolved.path, source)),
        };
        if existing.as_ref().is_some_and(std::fs::Metadata::is_dir) {
            return Err(error(
                ExecutionErrorCode::IsDirectory,
                "cannot write bytes to a directory",
            ));
        }

        match &request.condition {
            WriteCondition::Any => {}
            WriteCondition::MustNotExist if existing.is_some() => {
                return Err(error(
                    ExecutionErrorCode::AlreadyExists,
                    "write target already exists",
                ));
            }
            WriteCondition::MustNotExist => {}
            WriteCondition::MatchRevision { revision } => {
                let Some(metadata) = existing.as_ref() else {
                    return Err(error(
                        ExecutionErrorCode::RevisionConflict,
                        "write target no longer exists",
                    ));
                };
                let current = if metadata.file_type().is_symlink() {
                    let target = tokio::fs::read_link(&resolved.path)
                        .await
                        .map_err(|source| io_error(&resolved.path, source))?;
                    hash_bytes(target.to_string_lossy().as_bytes())
                } else if metadata.is_file() {
                    hash_file(&resolved.path).await?
                } else {
                    return Err(invalid_request(
                        "revision conditions require a regular file or symbolic link",
                    ));
                };
                if current != *revision {
                    return Err(error(
                        ExecutionErrorCode::RevisionConflict,
                        "write target revision changed",
                    ));
                }
            }
        }

        if request.create_parents {
            if let Some(parent) = resolved.path.parent() {
                tokio::fs::create_dir_all(parent)
                    .await
                    .map_err(|source| io_error(parent, source))?;
            }
        }
        let preserved_metadata = existing.as_ref().filter(|metadata| metadata.is_file());
        atomic_write(&resolved.path, request.data.as_slice(), preserved_metadata).await?;
        Ok(WriteFileResult {
            path: request.path.clone(),
            existed: existing.is_some(),
            revision: hash_bytes(request.data.as_slice()),
            bytes_written: byte_count,
        })
    }

    async fn create_directory_inner(
        &self,
        request: &CreateDirectoryRequest,
    ) -> ExecutionResult<()> {
        let resolved = self
            .resolver
            .resolve_for_write(&request.path, false)
            .await?;
        let result = if request.recursive {
            tokio::fs::create_dir_all(&resolved.path).await
        } else {
            tokio::fs::create_dir(&resolved.path).await
        };
        result.map_err(|source| io_error(&resolved.path, source))
    }

    async fn remove_inner(&self, request: &RemovePathRequest) -> ExecutionResult<RemovePathResult> {
        if request.path.path == "." {
            return Err(invalid_request("an execution root cannot be removed"));
        }
        let resolved = self
            .resolver
            .resolve_for_write(&request.path, false)
            .await?;
        let metadata = match tokio::fs::symlink_metadata(&resolved.path).await {
            Ok(metadata) => metadata,
            Err(source)
                if request.ignore_missing && source.kind() == std::io::ErrorKind::NotFound =>
            {
                return Ok(RemovePathResult { removed: false });
            }
            Err(source) => return Err(io_error(&resolved.path, source)),
        };

        if let Some(expected) = &request.expected_revision {
            if metadata.is_dir() {
                return Err(invalid_request(
                    "directory removal does not support expected_revision",
                ));
            }
            let current = if metadata.file_type().is_symlink() {
                let target = tokio::fs::read_link(&resolved.path)
                    .await
                    .map_err(|source| io_error(&resolved.path, source))?;
                hash_bytes(target.to_string_lossy().as_bytes())
            } else {
                hash_file(&resolved.path).await?
            };
            if &current != expected {
                return Err(error(
                    ExecutionErrorCode::RevisionConflict,
                    "remove target revision changed",
                ));
            }
        }

        let result = if metadata.is_dir() {
            if request.recursive {
                tokio::fs::remove_dir_all(&resolved.path).await
            } else {
                tokio::fs::remove_dir(&resolved.path).await
            }
        } else {
            tokio::fs::remove_file(&resolved.path).await
        };
        result
            .map(|()| RemovePathResult { removed: true })
            .map_err(|source| io_error(&resolved.path, source))
    }
}

#[async_trait]
impl FileSystem for SupervisorFileSystem {
    async fn stat(
        &self,
        context: &OperationContext,
        request: StatRequest,
    ) -> ExecutionResult<FileMetadata> {
        context.checkpoint()?;
        request.validate()?;
        let resolved = self
            .resolver
            .resolve_existing(&request.path, request.follow_symlinks)
            .await?;
        self.metadata_for(&request.path, &resolved.path, request.follow_symlinks)
            .await
    }

    async fn read(
        &self,
        context: &OperationContext,
        request: ReadFileRequest,
    ) -> ExecutionResult<ReadFileResult> {
        context.checkpoint()?;
        request.validate()?;
        if request.max_bytes > self.limits.max_read_bytes {
            return Err(error(
                ExecutionErrorCode::ResourceExhausted,
                format!(
                    "read requested {} bytes but the host limit is {}",
                    request.max_bytes, self.limits.max_read_bytes
                ),
            ));
        }
        let resolved = self
            .resolver
            .resolve_existing(&request.path, request.follow_symlinks)
            .await?;
        let native_metadata = if request.follow_symlinks {
            tokio::fs::metadata(&resolved.path).await
        } else {
            tokio::fs::symlink_metadata(&resolved.path).await
        }
        .map_err(|source| io_error(&resolved.path, source))?;
        if !native_metadata.is_file() {
            return Err(error(
                if native_metadata.is_dir() {
                    ExecutionErrorCode::IsDirectory
                } else {
                    ExecutionErrorCode::InvalidRequest
                },
                "byte reads require a regular file",
            ));
        }

        let mut file = tokio::fs::File::open(&resolved.path)
            .await
            .map_err(|source| io_error(&resolved.path, source))?;
        let mut hasher = Sha256::new();
        let mut selected = Vec::new();
        let mut position = 0_u64;
        let selection_end = request.offset.saturating_add(request.max_bytes);
        let mut buffer = vec![0_u8; 64 * 1024];
        loop {
            context.checkpoint()?;
            let read = file
                .read(&mut buffer)
                .await
                .map_err(|source| io_error(&resolved.path, source))?;
            if read == 0 {
                break;
            }
            let chunk = &buffer[..read];
            hasher.update(chunk);
            let chunk_start = position;
            let chunk_end = position + u64::try_from(read).unwrap_or(u64::MAX);
            if chunk_end > request.offset && chunk_start < selection_end {
                let start = request.offset.saturating_sub(chunk_start);
                let end = selection_end.min(chunk_end).saturating_sub(chunk_start);
                let start = usize::try_from(start).unwrap_or(read);
                let end = usize::try_from(end).unwrap_or(read).min(read);
                if start < end {
                    selected.extend_from_slice(&chunk[start..end]);
                }
            }
            position = chunk_end;
        }
        if request.offset > position {
            return Err(invalid_request("read offset exceeds file size"));
        }

        let metadata = FileMetadata {
            path: request.path,
            kind: FileKind::File,
            size: position,
            modified_at: modified_at(&native_metadata),
            revision: Some(
                FileRevision::new(format!("{:x}", hasher.finalize()))
                    .expect("SHA-256 digest is non-empty"),
            ),
        };
        let bytes_returned = u64::try_from(selected.len()).unwrap_or(u64::MAX);
        Ok(ReadFileResult {
            metadata,
            data: BinaryData::new(selected),
            offset: request.offset,
            eof: request.offset.saturating_add(bytes_returned) >= position,
        })
    }

    async fn write(
        &self,
        context: &OperationContext,
        request: WriteFileRequest,
    ) -> ExecutionResult<WriteFileResult> {
        context.checkpoint()?;
        request.validate()?;
        let fingerprint = fingerprint(&request)?;
        let mut state = self.mutation_state.lock().await;
        if let Some(record) = state.records.get(&request.operation_id) {
            ensure_fingerprint(record, &fingerprint, &request.operation_id)?;
            return match &record.result {
                MutationResult::Write(result) => Ok(result.clone()),
                MutationResult::CreateDirectory | MutationResult::Remove(_) => Err(
                    operation_conflict("operation ID was already used for another operation"),
                ),
            };
        }
        let result = self.write_inner(&request).await?;
        state.records.insert(
            request.operation_id,
            MutationRecord {
                fingerprint,
                result: MutationResult::Write(result.clone()),
            },
        );
        Ok(result)
    }

    async fn create_directory(
        &self,
        context: &OperationContext,
        request: CreateDirectoryRequest,
    ) -> ExecutionResult<()> {
        context.checkpoint()?;
        request.validate()?;
        let fingerprint = fingerprint(&request)?;
        let mut state = self.mutation_state.lock().await;
        if let Some(record) = state.records.get(&request.operation_id) {
            ensure_fingerprint(record, &fingerprint, &request.operation_id)?;
            return match &record.result {
                MutationResult::CreateDirectory => Ok(()),
                MutationResult::Write(_) | MutationResult::Remove(_) => Err(operation_conflict(
                    "operation ID was already used for another operation",
                )),
            };
        }
        self.create_directory_inner(&request).await?;
        state.records.insert(
            request.operation_id,
            MutationRecord {
                fingerprint,
                result: MutationResult::CreateDirectory,
            },
        );
        Ok(())
    }

    async fn remove(
        &self,
        context: &OperationContext,
        request: RemovePathRequest,
    ) -> ExecutionResult<RemovePathResult> {
        context.checkpoint()?;
        request.validate()?;
        let fingerprint = fingerprint(&request)?;
        let mut state = self.mutation_state.lock().await;
        if let Some(record) = state.records.get(&request.operation_id) {
            ensure_fingerprint(record, &fingerprint, &request.operation_id)?;
            return match &record.result {
                MutationResult::Remove(result) => Ok(result.clone()),
                MutationResult::Write(_) | MutationResult::CreateDirectory => Err(
                    operation_conflict("operation ID was already used for another operation"),
                ),
            };
        }
        let result = self.remove_inner(&request).await?;
        state.records.insert(
            request.operation_id,
            MutationRecord {
                fingerprint,
                result: MutationResult::Remove(result.clone()),
            },
        );
        Ok(result)
    }

    async fn list(
        &self,
        context: &OperationContext,
        request: ListDirectoryRequest,
    ) -> ExecutionResult<ListDirectoryResult> {
        context.checkpoint()?;
        request.validate()?;
        let resolved = self
            .resolver
            .resolve_existing(&request.path, request.follow_symlinks)
            .await?;
        let directory_metadata = tokio::fs::metadata(&resolved.path)
            .await
            .map_err(|source| io_error(&resolved.path, source))?;
        if !directory_metadata.is_dir() {
            return Err(error(
                ExecutionErrorCode::NotDirectory,
                "path is not a directory",
            ));
        }

        let mut reader = tokio::fs::read_dir(&resolved.path)
            .await
            .map_err(|source| io_error(&resolved.path, source))?;
        let mut names = Vec::new();
        while let Some(entry) = reader
            .next_entry()
            .await
            .map_err(|source| io_error(&resolved.path, source))?
        {
            names.push(entry.file_name().to_string_lossy().into_owned());
        }
        names.sort();
        if let Some(cursor) = &request.cursor {
            names.retain(|name| name.as_str() > cursor.as_str());
        }

        let maximum = usize::try_from(request.max_entries).unwrap_or(usize::MAX);
        let has_more = names.len() > maximum;
        names.truncate(maximum);
        let mut entries = Vec::with_capacity(names.len());
        for name in names {
            context.checkpoint()?;
            let portable_path = if request.path.path == "." {
                name.clone()
            } else {
                format!("{}/{}", request.path.path, name)
            };
            let child = ExecutionPath::new(request.path.root_id.clone(), portable_path)
                .expect("directory entry creates a valid child path");
            let resolved_child = self
                .resolver
                .resolve_existing(&child, request.follow_symlinks)
                .await?;
            let metadata = self
                .metadata_for(&child, &resolved_child.path, request.follow_symlinks)
                .await?;
            entries.push(DirectoryEntry { name, metadata });
        }
        let next_cursor = if has_more {
            entries.last().map(|entry| {
                DirectoryCursor::new(entry.name.clone())
                    .expect("directory entries always have non-empty names")
            })
        } else {
            None
        };
        Ok(ListDirectoryResult {
            entries,
            next_cursor,
        })
    }
}

fn ensure_fingerprint(
    record: &MutationRecord,
    fingerprint: &str,
    operation_id: &OperationId,
) -> ExecutionResult<()> {
    if record.fingerprint == fingerprint {
        Ok(())
    } else {
        Err(operation_conflict(format!(
            "operation ID `{operation_id}` was reused with a different request"
        )))
    }
}

fn fingerprint(value: &impl Serialize) -> ExecutionResult<String> {
    let encoded = serde_json::to_vec(value).map_err(|source| {
        error(
            ExecutionErrorCode::Internal,
            format!("failed to fingerprint operation: {source}"),
        )
    })?;
    Ok(format!("{:x}", Sha256::digest(encoded)))
}

fn file_kind(metadata: &std::fs::Metadata) -> FileKind {
    let kind = metadata.file_type();
    if kind.is_file() {
        FileKind::File
    } else if kind.is_dir() {
        FileKind::Directory
    } else if kind.is_symlink() {
        FileKind::Symlink
    } else {
        FileKind::Other
    }
}

fn modified_at(metadata: &std::fs::Metadata) -> Option<TimestampMs> {
    metadata
        .modified()
        .ok()?
        .duration_since(UNIX_EPOCH)
        .ok()
        .and_then(|duration| u64::try_from(duration.as_millis()).ok())
        .map(TimestampMs)
}

async fn hash_file(path: &Path) -> ExecutionResult<FileRevision> {
    let mut file = tokio::fs::File::open(path)
        .await
        .map_err(|source| io_error(path, source))?;
    let mut hasher = Sha256::new();
    let mut buffer = vec![0_u8; 64 * 1024];
    loop {
        let read = file
            .read(&mut buffer)
            .await
            .map_err(|source| io_error(path, source))?;
        if read == 0 {
            break;
        }
        hasher.update(&buffer[..read]);
    }
    Ok(FileRevision::new(format!("{:x}", hasher.finalize())).expect("SHA-256 digest is non-empty"))
}

fn hash_bytes(bytes: &[u8]) -> FileRevision {
    FileRevision::new(format!("{:x}", Sha256::digest(bytes))).expect("SHA-256 digest is non-empty")
}

async fn atomic_write(
    path: &Path,
    contents: &[u8],
    existing: Option<&std::fs::Metadata>,
) -> ExecutionResult<()> {
    let parent = path
        .parent()
        .ok_or_else(|| invalid_request("write target has no parent directory"))?;
    let file_name = path
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or("file");
    let temporary = temporary_path(parent, file_name);
    let mut options = tokio::fs::OpenOptions::new();
    options.create_new(true).write(true);
    let mut file = options
        .open(&temporary)
        .await
        .map_err(|source| io_error(&temporary, source))?;
    let result = async {
        file.write_all(contents)
            .await
            .map_err(|source| io_error(&temporary, source))?;
        if let Some(metadata) = existing {
            file.set_permissions(metadata.permissions())
                .await
                .map_err(|source| io_error(&temporary, source))?;
        }
        file.sync_all()
            .await
            .map_err(|source| io_error(&temporary, source))?;
        drop(file);
        tokio::fs::rename(&temporary, path)
            .await
            .map_err(|source| io_error(path, source))
    }
    .await;
    if result.is_err() {
        let _ = tokio::fs::remove_file(&temporary).await;
    }
    result
}

fn temporary_path(parent: &Path, file_name: &str) -> PathBuf {
    parent.join(format!(".{file_name}.{}.tmp", Uuid::now_v7()))
}
