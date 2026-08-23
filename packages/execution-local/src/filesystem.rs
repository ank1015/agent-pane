use std::{path::Path, sync::Arc};

use async_trait::async_trait;
use base64::{Engine, engine::general_purpose::STANDARD};
use execution_contracts::{
    ArtifactKind, BinaryContent, ContentSource, CopyPathRequest, CreateDirectoryRequest,
    DirectoryEntry, ExecutionErrorCode, FileKind, FileMetadata, FileRevision, InspectRequest,
    ListRequest, ListResult, ListSort, MovePathRequest, PathSpec, ReadBytesRequest,
    ReadBytesResult, RemovePathRequest, TimestampMs, Validate, WalkRequest, WalkResult,
    WriteBytesRequest, WriteBytesResult, WriteCondition,
};
use execution_runtime::{BasicFileSystem, ExecutionResult, OperationContext};
use tokio::io::{AsyncReadExt, AsyncSeekExt};
use uuid::Uuid;

use crate::{
    artifacts::{LocalArtifactStore, hex_digest},
    error::{error, io_error},
    path_resolver::PathResolver,
};

const MAX_INLINE_BYTES: usize = 256 * 1024;

#[derive(Clone)]
pub(crate) struct LocalFileSystem {
    resolver: Arc<PathResolver>,
    artifacts: Arc<LocalArtifactStore>,
}

impl LocalFileSystem {
    pub fn new(resolver: Arc<PathResolver>, artifacts: Arc<LocalArtifactStore>) -> Self {
        Self {
            resolver,
            artifacts,
        }
    }

    pub async fn metadata_for(
        &self,
        spec: &PathSpec,
        follow_symlinks: bool,
    ) -> ExecutionResult<FileMetadata> {
        let resolved = self
            .resolver
            .resolve_existing(spec, follow_symlinks)
            .await?;
        let metadata = if follow_symlinks {
            tokio::fs::metadata(&resolved.path).await
        } else {
            tokio::fs::symlink_metadata(&resolved.path).await
        }
        .map_err(|source| io_error(&resolved.path, source))?;
        metadata_from_std(spec.clone(), &resolved.path, metadata).await
    }

    pub async fn content_bytes(&self, source: &ContentSource) -> ExecutionResult<Vec<u8>> {
        match source {
            ContentSource::Text { content } => Ok(content.as_bytes().to_vec()),
            ContentSource::Base64 { data, .. } => STANDARD.decode(&data.0).map_err(|source| {
                error(
                    ExecutionErrorCode::InvalidRequest,
                    format!("invalid base64 content: {source}"),
                )
            }),
            ContentSource::Artifact { artifact_id } => self.artifacts.read_all(artifact_id).await,
        }
    }

    pub async fn current_revision(&self, path: &Path) -> ExecutionResult<FileRevision> {
        let bytes = tokio::fs::read(path)
            .await
            .map_err(|source| io_error(path, source))?;
        Ok(FileRevision::new(hex_digest(&bytes)).expect("SHA-256 digest is non-empty"))
    }

    async fn write_internal(
        &self,
        request: WriteBytesRequest,
    ) -> ExecutionResult<WriteBytesResult> {
        let resolved = self
            .resolver
            .resolve_for_write(&request.path, request.follow_symlinks)
            .await?;
        let existing = tokio::fs::symlink_metadata(&resolved.path).await.ok();
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
                    "target already exists",
                ));
            }
            WriteCondition::MatchRevision { revision } => {
                if existing.is_none() {
                    return Err(error(
                        ExecutionErrorCode::StaleRevision,
                        "target no longer exists",
                    ));
                }
                if self.current_revision(&resolved.path).await? != *revision {
                    return Err(error(
                        ExecutionErrorCode::StaleRevision,
                        "target revision changed",
                    ));
                }
            }
            WriteCondition::MustNotExist => {}
        }

        let bytes = self.content_bytes(&request.content).await?;
        if request.create_parents {
            if let Some(parent) = resolved.path.parent() {
                tokio::fs::create_dir_all(parent)
                    .await
                    .map_err(|source| io_error(parent, source))?;
            }
        }
        if request.atomic_replace {
            atomic_write(&resolved.path, &bytes).await?;
        } else {
            tokio::fs::write(&resolved.path, &bytes)
                .await
                .map_err(|source| io_error(&resolved.path, source))?;
        }
        Ok(WriteBytesResult {
            path: request.path,
            existed: existing.is_some(),
            revision: FileRevision::new(hex_digest(&bytes)).expect("SHA-256 digest is non-empty"),
            bytes_written: bytes.len() as u64,
        })
    }
}

#[async_trait]
impl BasicFileSystem for LocalFileSystem {
    async fn inspect(
        &self,
        _context: &OperationContext,
        request: InspectRequest,
    ) -> ExecutionResult<FileMetadata> {
        self.metadata_for(&request.path, request.follow_symlinks)
            .await
    }

    async fn read_bytes(
        &self,
        _context: &OperationContext,
        request: ReadBytesRequest,
    ) -> ExecutionResult<ReadBytesResult> {
        request.validate().map_err(invalid_request)?;
        let metadata = self
            .metadata_for(&request.path, request.follow_symlinks)
            .await?;
        if metadata.kind != FileKind::File {
            return Err(error(
                ExecutionErrorCode::IsDirectory,
                "byte reads require a regular file",
            ));
        }
        let resolved = self
            .resolver
            .resolve_existing(&request.path, request.follow_symlinks)
            .await?;
        let offset = request.range.map_or(0, |range| range.offset);
        if offset > metadata.size {
            return Err(error(
                ExecutionErrorCode::InvalidRequest,
                "read offset exceeds file size",
            ));
        }
        let requested = request
            .range
            .map_or(metadata.size - offset, |range| range.length)
            .min(metadata.size - offset);
        let requested_usize = usize::try_from(requested).map_err(|_| {
            error(
                ExecutionErrorCode::ResourceExhausted,
                "requested byte range does not fit in memory",
            )
        })?;
        let mut file = tokio::fs::File::open(&resolved.path)
            .await
            .map_err(|source| io_error(&resolved.path, source))?;
        file.seek(std::io::SeekFrom::Start(offset))
            .await
            .map_err(|source| io_error(&resolved.path, source))?;
        let mut bytes = vec![0; requested_usize];
        file.read_exact(&mut bytes)
            .await
            .map_err(|source| io_error(&resolved.path, source))?;
        let content = if bytes.len() <= MAX_INLINE_BYTES {
            BinaryContent::Base64 {
                data: execution_contracts::Base64Data(STANDARD.encode(&bytes)),
                mime_type: metadata.mime_type.clone(),
            }
        } else {
            let artifact = self
                .artifacts
                .put(
                    ArtifactKind::File,
                    file_name(&resolved.path),
                    metadata.mime_type.clone(),
                    &bytes,
                )
                .await?;
            BinaryContent::Artifact {
                artifact_id: artifact.artifact_id,
            }
        };
        let file_size = metadata.size;
        Ok(ReadBytesResult {
            metadata,
            content,
            offset,
            bytes_returned: bytes.len() as u64,
            eof: offset + bytes.len() as u64 >= file_size,
        })
    }

    async fn write_bytes(
        &self,
        _context: &OperationContext,
        request: WriteBytesRequest,
    ) -> ExecutionResult<WriteBytesResult> {
        self.write_internal(request).await
    }

    async fn create_directory(
        &self,
        _context: &OperationContext,
        request: CreateDirectoryRequest,
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

    async fn remove(
        &self,
        _context: &OperationContext,
        request: RemovePathRequest,
    ) -> ExecutionResult<()> {
        let resolved = match self.resolver.resolve_for_write(&request.path, false).await {
            Ok(resolved) => resolved,
            Err(source) if request.force && source.code == ExecutionErrorCode::NotFound => {
                return Ok(());
            }
            Err(source) => return Err(source),
        };
        if let Some(expected) = request.expected_revision {
            if self.current_revision(&resolved.path).await? != expected {
                return Err(error(
                    ExecutionErrorCode::StaleRevision,
                    "target revision changed",
                ));
            }
        }
        let metadata = match tokio::fs::symlink_metadata(&resolved.path).await {
            Ok(metadata) => metadata,
            Err(source) if request.force && source.kind() == std::io::ErrorKind::NotFound => {
                return Ok(());
            }
            Err(source) => return Err(io_error(&resolved.path, source)),
        };
        let result = if metadata.is_dir() {
            if request.recursive {
                tokio::fs::remove_dir_all(&resolved.path).await
            } else {
                tokio::fs::remove_dir(&resolved.path).await
            }
        } else {
            tokio::fs::remove_file(&resolved.path).await
        };
        result.map_err(|source| io_error(&resolved.path, source))
    }

    async fn move_path(
        &self,
        _context: &OperationContext,
        request: MovePathRequest,
    ) -> ExecutionResult<()> {
        let source = self
            .resolver
            .resolve_for_write(&request.source, false)
            .await?;
        let destination = self
            .resolver
            .resolve_for_write(&request.destination, false)
            .await?;
        if let Some(expected) = request.expected_source_revision {
            if self.current_revision(&source.path).await? != expected {
                return Err(error(
                    ExecutionErrorCode::StaleRevision,
                    "source revision changed",
                ));
            }
        }
        if !request.overwrite && tokio::fs::symlink_metadata(&destination.path).await.is_ok() {
            return Err(error(
                ExecutionErrorCode::AlreadyExists,
                "destination already exists",
            ));
        }
        tokio::fs::rename(&source.path, &destination.path)
            .await
            .map_err(|source| io_error(&destination.path, source))
    }

    async fn copy_path(
        &self,
        _context: &OperationContext,
        request: CopyPathRequest,
    ) -> ExecutionResult<()> {
        let source = self
            .resolver
            .resolve_existing(&request.source, false)
            .await?;
        let destination = self
            .resolver
            .resolve_for_write(&request.destination, false)
            .await?;
        if !request.overwrite && tokio::fs::symlink_metadata(&destination.path).await.is_ok() {
            return Err(error(
                ExecutionErrorCode::AlreadyExists,
                "destination already exists",
            ));
        }
        let metadata = tokio::fs::symlink_metadata(&source.path)
            .await
            .map_err(|source_error| io_error(&source.path, source_error))?;
        if metadata.is_dir() {
            if !request.recursive {
                return Err(error(
                    ExecutionErrorCode::IsDirectory,
                    "recursive must be enabled to copy a directory",
                ));
            }
            let source_path = source.path.clone();
            let destination_path = destination.path.clone();
            tokio::task::spawn_blocking(move || copy_directory(&source_path, &destination_path))
                .await
                .map_err(|join| error(ExecutionErrorCode::Internal, join.to_string()))?
                .map_err(|source_error| io_error(&destination.path, source_error))?;
        } else {
            tokio::fs::copy(&source.path, &destination.path)
                .await
                .map_err(|source_error| io_error(&destination.path, source_error))?;
        }
        Ok(())
    }

    async fn list_raw(
        &self,
        _context: &OperationContext,
        request: ListRequest,
    ) -> ExecutionResult<ListResult> {
        let directory = self
            .metadata_for(&request.path, request.follow_symlinks)
            .await?;
        if directory.kind != FileKind::Directory {
            return Err(error(
                ExecutionErrorCode::NotDirectory,
                "path is not a directory",
            ));
        }
        let resolved = self
            .resolver
            .resolve_existing(&request.path, request.follow_symlinks)
            .await?;
        let mut reader = tokio::fs::read_dir(&resolved.path)
            .await
            .map_err(|source| io_error(&resolved.path, source))?;
        let mut entries = Vec::new();
        while let Some(entry) = reader
            .next_entry()
            .await
            .map_err(|source| io_error(&resolved.path, source))?
        {
            let name = entry.file_name().to_string_lossy().into_owned();
            if !request.include_hidden && name.starts_with('.') {
                continue;
            }
            let child_spec = child_path(&request.path, &name);
            let metadata = self
                .metadata_for(&child_spec, request.follow_symlinks)
                .await?;
            entries.push(DirectoryEntry {
                path: child_spec,
                name,
                kind: metadata.kind,
                size: (metadata.kind == FileKind::File).then_some(metadata.size),
                modified_at: metadata.modified_at,
            });
        }
        sort_entries(&mut entries, request.sort);
        let start = decode_index(request.cursor.as_ref())?;
        let end = (start + request.limit as usize).min(entries.len());
        let truncated = end < entries.len();
        let page = entries.get(start..end).unwrap_or(&[]).to_vec();
        Ok(ListResult {
            directory,
            entries: page,
            truncated,
            cursor: truncated.then(|| index_cursor(end)),
        })
    }

    async fn walk(
        &self,
        _context: &OperationContext,
        request: WalkRequest,
    ) -> ExecutionResult<WalkResult> {
        request.validate().map_err(invalid_request)?;
        let resolved = self.resolver.resolve_existing(&request.root, false).await?;
        let root_spec = request.root.clone();
        let root_path = resolved.path.clone();
        let include_hidden = request.include_hidden;
        let follow = request.follow_directory_symlinks;
        let max_depth = request.max_depth as usize;
        let values = tokio::task::spawn_blocking(move || {
            let mut builder = ignore::WalkBuilder::new(&root_path);
            builder
                .hidden(!include_hidden)
                .follow_links(follow)
                .max_depth(Some(max_depth));
            builder
                .build()
                .skip(1)
                .filter_map(Result::ok)
                .filter_map(|entry| {
                    let relative = entry.path().strip_prefix(&root_path).ok()?;
                    let relative = relative.to_string_lossy().replace('\\', "/");
                    let spec = append_relative(&root_spec, &relative);
                    let file_type = entry.file_type()?;
                    let kind = if file_type.is_file() {
                        FileKind::File
                    } else if file_type.is_dir() {
                        FileKind::Directory
                    } else if file_type.is_symlink() {
                        FileKind::Symlink
                    } else {
                        FileKind::Other
                    };
                    let metadata = entry.metadata().ok();
                    Some(DirectoryEntry {
                        path: spec,
                        name: entry.file_name().to_string_lossy().into_owned(),
                        kind,
                        size: metadata
                            .as_ref()
                            .filter(|_| kind == FileKind::File)
                            .map(std::fs::Metadata::len),
                        modified_at: metadata.as_ref().and_then(modified_at),
                    })
                })
                .collect::<Vec<_>>()
        })
        .await
        .map_err(|join| error(ExecutionErrorCode::Internal, join.to_string()))?;
        let start = decode_index(request.cursor.as_ref())?;
        let end = (start + request.max_entries as usize).min(values.len());
        let truncated = end < values.len();
        Ok(WalkResult {
            entries: values.get(start..end).unwrap_or(&[]).to_vec(),
            truncated,
            cursor: truncated.then(|| index_cursor(end)),
        })
    }
}

async fn atomic_write(path: &Path, bytes: &[u8]) -> ExecutionResult<()> {
    let parent = path.parent().ok_or_else(|| {
        error(
            ExecutionErrorCode::InvalidPath,
            "target has no parent directory",
        )
    })?;
    let temporary = parent.join(format!(".execution-{}.tmp", Uuid::now_v7()));
    let mut options = tokio::fs::OpenOptions::new();
    options.write(true).create_new(true);
    let mut file = options
        .open(&temporary)
        .await
        .map_err(|source| io_error(&temporary, source))?;
    use tokio::io::AsyncWriteExt;
    if let Err(source) = file.write_all(bytes).await {
        let _ = tokio::fs::remove_file(&temporary).await;
        return Err(io_error(&temporary, source));
    }
    if let Err(source) = file.sync_all().await {
        let _ = tokio::fs::remove_file(&temporary).await;
        return Err(io_error(&temporary, source));
    }
    drop(file);
    if let Err(source) = tokio::fs::rename(&temporary, path).await {
        let _ = tokio::fs::remove_file(&temporary).await;
        return Err(io_error(path, source));
    }
    Ok(())
}

async fn metadata_from_std(
    spec: PathSpec,
    path: &Path,
    metadata: std::fs::Metadata,
) -> ExecutionResult<FileMetadata> {
    let kind = if metadata.file_type().is_symlink() {
        FileKind::Symlink
    } else if metadata.is_file() {
        FileKind::File
    } else if metadata.is_dir() {
        FileKind::Directory
    } else {
        FileKind::Other
    };
    let revision = if kind == FileKind::File {
        let bytes = tokio::fs::read(path)
            .await
            .map_err(|source| io_error(path, source))?;
        Some(FileRevision::new(hex_digest(&bytes)).expect("SHA-256 digest is non-empty"))
    } else {
        None
    };
    Ok(FileMetadata {
        path: spec,
        kind,
        size: metadata.len(),
        created_at: metadata.created().ok().and_then(system_time),
        modified_at: metadata.modified().ok().and_then(system_time),
        revision,
        mime_type: mime_type(path),
    })
}

fn system_time(value: std::time::SystemTime) -> Option<TimestampMs> {
    value
        .duration_since(std::time::UNIX_EPOCH)
        .ok()
        .and_then(|duration| u64::try_from(duration.as_millis()).ok())
        .map(TimestampMs)
}

fn modified_at(metadata: &std::fs::Metadata) -> Option<TimestampMs> {
    metadata.modified().ok().and_then(system_time)
}

pub(crate) fn mime_type(path: &Path) -> Option<String> {
    let extension = path.extension()?.to_str()?.to_ascii_lowercase();
    let mime = match extension.as_str() {
        "txt" | "md" | "rs" | "js" | "ts" | "tsx" | "jsx" | "json" | "toml" | "yaml" | "yml"
        | "py" | "go" | "java" | "c" | "h" | "cpp" | "css" | "html" => "text/plain",
        "png" => "image/png",
        "jpg" | "jpeg" => "image/jpeg",
        "gif" => "image/gif",
        "webp" => "image/webp",
        "pdf" => "application/pdf",
        "zip" => "application/zip",
        _ => return None,
    };
    Some(mime.to_owned())
}

fn file_name(path: &Path) -> Option<String> {
    path.file_name()
        .map(|name| name.to_string_lossy().into_owned())
}

fn child_path(parent: &PathSpec, name: &str) -> PathSpec {
    append_relative(parent, name)
}

pub(crate) fn append_relative(parent: &PathSpec, relative: &str) -> PathSpec {
    match parent {
        PathSpec::Workspace { root_id, path } => PathSpec::Workspace {
            root_id: root_id.clone(),
            path: if path == "." || path.is_empty() {
                relative.to_owned()
            } else {
                format!("{path}/{relative}")
            },
        },
        PathSpec::Native { uri, grant_id } => PathSpec::Native {
            uri: format!("{}/{relative}", uri.trim_end_matches('/')),
            grant_id: grant_id.clone(),
        },
    }
}

fn sort_entries(entries: &mut [DirectoryEntry], sort: ListSort) {
    match sort {
        ListSort::NameAscending => entries.sort_by(|a, b| a.name.cmp(&b.name)),
        ListSort::NameDescending => entries.sort_by(|a, b| b.name.cmp(&a.name)),
        ListSort::ModifiedAscending => entries.sort_by_key(|entry| entry.modified_at),
        ListSort::ModifiedDescending => {
            entries.sort_by_key(|entry| std::cmp::Reverse(entry.modified_at));
        }
    }
}

fn index_cursor(index: usize) -> execution_contracts::ContinuationCursor {
    execution_contracts::ContinuationCursor::new(format!("index:{index}"))
        .expect("index cursor is non-empty")
}

fn decode_index(
    cursor: Option<&execution_contracts::ContinuationCursor>,
) -> ExecutionResult<usize> {
    let Some(cursor) = cursor else {
        return Ok(0);
    };
    cursor
        .as_str()
        .strip_prefix("index:")
        .and_then(|value| value.parse().ok())
        .ok_or_else(|| {
            error(
                ExecutionErrorCode::InvalidRequest,
                "invalid pagination cursor",
            )
        })
}

fn copy_directory(source: &Path, destination: &Path) -> std::io::Result<()> {
    std::fs::create_dir_all(destination)?;
    for entry in std::fs::read_dir(source)? {
        let entry = entry?;
        let destination = destination.join(entry.file_name());
        if entry.file_type()?.is_dir() {
            copy_directory(&entry.path(), &destination)?;
        } else {
            std::fs::copy(entry.path(), destination)?;
        }
    }
    Ok(())
}

fn invalid_request(
    source: execution_contracts::ValidationError,
) -> execution_contracts::ExecutionError {
    error(
        ExecutionErrorCode::InvalidRequest,
        format!("invalid filesystem request: {source}"),
    )
}
