use std::{collections::VecDeque, path::Path, sync::Arc};

use async_trait::async_trait;
use execution_contracts::{
    ArtifactKind, ArtifactRead, ContinuationCursor, DirectoryEntry, ExecutionErrorCode, FileKind,
    FileMetadata, IgnoreMode, InspectManyRequest, InspectManyResult, InspectRequest, ItemOutcome,
    ListRequest, ListResult, ReadMode, ReadRequest, ReadResult, SearchItem, SearchKind,
    SearchRequest, SearchResult, SearchSyntax, TextMatch, TextPage, TextPageRequest, TextSubmatch,
    Validate,
};
use execution_runtime::{ExecutionResult, OperationContext, WorkspaceQuery};
use globset::{Glob, GlobSet, GlobSetBuilder};
use regex::Regex;

use crate::{
    artifacts::LocalArtifactStore,
    error::{error, io_error},
    filesystem::{LocalFileSystem, append_relative},
    path_resolver::PathResolver,
};

pub(crate) struct LocalWorkspaceQuery {
    resolver: Arc<PathResolver>,
    artifacts: Arc<LocalArtifactStore>,
    filesystem: LocalFileSystem,
}

impl LocalWorkspaceQuery {
    pub fn new(resolver: Arc<PathResolver>, artifacts: Arc<LocalArtifactStore>) -> Self {
        Self {
            filesystem: LocalFileSystem::new(Arc::clone(&resolver), Arc::clone(&artifacts)),
            resolver,
            artifacts,
        }
    }

    async fn read_text(
        &self,
        request: &ReadRequest,
        metadata: FileMetadata,
        local_path: &Path,
    ) -> ExecutionResult<ReadResult> {
        let bytes = tokio::fs::read(local_path)
            .await
            .map_err(|source| io_error(local_path, source))?;
        let text = String::from_utf8(bytes).map_err(|_| {
            error(
                ExecutionErrorCode::InvalidRequest,
                "file is not valid UTF-8; use bytes mode",
            )
        })?;
        let bounds = request.page.clone().unwrap_or_default();
        let start_line = requested_line(&bounds)?;
        let all_lines: Vec<&str> = text.split_inclusive('\n').collect();
        let total_lines = if text.is_empty() {
            0
        } else {
            all_lines.len() as u64
        };
        let start_index = usize::try_from(start_line.saturating_sub(1)).unwrap_or(usize::MAX);
        let mut content = String::new();
        let mut returned = 0_u32;
        let mut lines_truncated = false;
        let mut next_index = start_index;

        while next_index < all_lines.len() && returned < bounds.max_lines {
            let original = all_lines[next_index];
            let line = truncate_utf8(original, bounds.max_line_bytes as usize);
            lines_truncated |= line.len() < original.len();
            if content.len() + line.len() > bounds.max_bytes as usize {
                if content.is_empty() {
                    let available = bounds.max_bytes as usize;
                    let truncated = truncate_utf8(line, available);
                    content.push_str(truncated);
                    lines_truncated = true;
                    returned += 1;
                    next_index += 1;
                }
                break;
            }
            content.push_str(line);
            returned += 1;
            next_index += 1;
        }

        let has_more = next_index < all_lines.len();
        let end_line = if returned == 0 {
            start_line
        } else {
            start_line + u64::from(returned) - 1
        };
        Ok(ReadResult::Text {
            page: TextPage {
                metadata,
                content,
                start_line,
                end_line,
                has_more,
                next_line: has_more.then_some(end_line + 1),
                cursor: has_more.then(|| line_cursor(end_line + 1)),
                total_lines: bounds.include_total_lines.then_some(total_lines),
                lines_truncated,
            },
        })
    }

    async fn read_artifact(
        &self,
        metadata: FileMetadata,
        local_path: &Path,
        media: bool,
    ) -> ExecutionResult<ReadResult> {
        let bytes = tokio::fs::read(local_path)
            .await
            .map_err(|source| io_error(local_path, source))?;
        let mime = metadata
            .mime_type
            .clone()
            .unwrap_or_else(|| "application/octet-stream".to_owned());
        let artifact = self
            .artifacts
            .put(
                ArtifactKind::File,
                local_path
                    .file_name()
                    .map(|name| name.to_string_lossy().into_owned()),
                Some(mime.clone()),
                &bytes,
            )
            .await?;
        let read = ArtifactRead {
            metadata,
            artifact_id: artifact.artifact_id,
            mime_type: mime,
        };
        if media {
            Ok(ReadResult::Media { artifact: read })
        } else {
            Ok(ReadResult::Binary { artifact: read })
        }
    }
}

#[async_trait]
impl WorkspaceQuery for LocalWorkspaceQuery {
    async fn inspect(
        &self,
        _context: &OperationContext,
        request: InspectRequest,
    ) -> ExecutionResult<FileMetadata> {
        self.filesystem
            .metadata_for(&request.path, request.follow_symlinks)
            .await
    }

    async fn inspect_many(
        &self,
        _context: &OperationContext,
        request: InspectManyRequest,
    ) -> ExecutionResult<InspectManyResult> {
        request.validate().map_err(invalid_request)?;
        let mut items = Vec::with_capacity(request.paths.len());
        for path in request.paths {
            match self
                .filesystem
                .metadata_for(&path, request.follow_symlinks)
                .await
            {
                Ok(value) => items.push(ItemOutcome::Success { value }),
                Err(error) => items.push(ItemOutcome::Error { error }),
            }
        }
        Ok(InspectManyResult { items })
    }

    async fn read(
        &self,
        _context: &OperationContext,
        request: ReadRequest,
    ) -> ExecutionResult<ReadResult> {
        request.validate().map_err(invalid_request)?;
        let metadata = self.filesystem.metadata_for(&request.path, true).await?;
        if metadata.kind == FileKind::Directory {
            return Err(error(
                ExecutionErrorCode::IsDirectory,
                "use list for directory paths",
            ));
        }
        if metadata.kind != FileKind::File {
            return Err(error(
                ExecutionErrorCode::InvalidRequest,
                "read requires a regular file",
            ));
        }
        let resolved = self.resolver.resolve_existing(&request.path, true).await?;
        match request.mode {
            ReadMode::Text => self.read_text(&request, metadata, &resolved.path).await,
            ReadMode::Bytes => self.read_artifact(metadata, &resolved.path, false).await,
            ReadMode::Media => self.read_artifact(metadata, &resolved.path, true).await,
            ReadMode::Auto => {
                let mime = metadata.mime_type.as_deref().unwrap_or_default();
                if mime.starts_with("image/") || mime == "application/pdf" {
                    self.read_artifact(metadata, &resolved.path, true).await
                } else {
                    let sample = tokio::fs::read(&resolved.path)
                        .await
                        .map_err(|source| io_error(&resolved.path, source))?;
                    if std::str::from_utf8(&sample).is_ok() {
                        self.read_text(&request, metadata, &resolved.path).await
                    } else {
                        self.read_artifact(metadata, &resolved.path, false).await
                    }
                }
            }
        }
    }

    async fn list(
        &self,
        context: &OperationContext,
        request: ListRequest,
    ) -> ExecutionResult<ListResult> {
        request.validate().map_err(invalid_request)?;
        execution_runtime::BasicFileSystem::list_raw(&self.filesystem, context, request).await
    }

    async fn search(
        &self,
        context: &OperationContext,
        request: SearchRequest,
    ) -> ExecutionResult<SearchResult> {
        request.validate().map_err(invalid_request)?;
        if context.is_cancelled() {
            return Err(error(ExecutionErrorCode::Cancelled, "search was cancelled"));
        }
        let resolved = self.resolver.resolve_existing(&request.root, false).await?;
        let root_path = resolved.path;
        let request_for_worker = request.clone();
        let all_items =
            tokio::task::spawn_blocking(move || search_blocking(&root_path, &request_for_worker))
                .await
                .map_err(|join| error(ExecutionErrorCode::Internal, join.to_string()))??;

        let start = decode_search_cursor(request.cursor.as_ref())?;
        let mut bytes = 0_u64;
        let mut page = Vec::new();
        let mut next = start;
        while next < all_items.len() && page.len() < request.max_results as usize {
            let item_bytes = estimated_item_bytes(&all_items[next]);
            if !page.is_empty() && bytes + item_bytes > request.max_bytes {
                break;
            }
            bytes += item_bytes;
            page.push(all_items[next].clone());
            next += 1;
        }
        let truncated = next < all_items.len();
        Ok(SearchResult {
            items: page,
            truncated,
            cursor: truncated.then(|| search_cursor(next)),
            snapshot_revision: None,
        })
    }
}

fn search_blocking(root: &Path, request: &SearchRequest) -> ExecutionResult<Vec<SearchItem>> {
    let include = build_glob_set(&request.include)?;
    let exclude = build_glob_set(&request.exclude)?;
    let matcher = SearchMatcher::new(request)?;
    let mut builder = ignore::WalkBuilder::new(root);
    builder
        .hidden(!request.hidden)
        .follow_links(request.follow_symlinks);
    if request.ignore_mode == IgnoreMode::None {
        builder
            .ignore(false)
            .git_ignore(false)
            .git_exclude(false)
            .parents(false);
    }
    let mut items = Vec::new();
    for entry in builder.build().skip(1).filter_map(Result::ok) {
        let relative = match entry.path().strip_prefix(root) {
            Ok(path) => path.to_string_lossy().replace('\\', "/"),
            Err(_) => continue,
        };
        if include.as_ref().is_some_and(|set| !set.is_match(&relative))
            || exclude.as_ref().is_some_and(|set| set.is_match(&relative))
        {
            continue;
        }
        let spec = append_relative(&request.root, &relative);
        if request.kind == SearchKind::Paths {
            if matcher.is_match(&relative).is_none() {
                continue;
            }
            let Some(file_type) = entry.file_type() else {
                continue;
            };
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
            items.push(SearchItem::Path {
                entry: DirectoryEntry {
                    path: spec,
                    name: entry.file_name().to_string_lossy().into_owned(),
                    kind,
                    size: metadata
                        .as_ref()
                        .filter(|_| kind == FileKind::File)
                        .map(std::fs::Metadata::len),
                    modified_at: metadata
                        .as_ref()
                        .and_then(|metadata| metadata.modified().ok())
                        .and_then(timestamp),
                },
            });
            continue;
        }
        if !entry.file_type().is_some_and(|kind| kind.is_file()) {
            continue;
        }
        let Ok(content) = std::fs::read_to_string(entry.path()) else {
            continue;
        };
        let lines: Vec<&str> = content.lines().collect();
        let mut byte_offset = 0_u64;
        for (index, line) in lines.iter().enumerate() {
            if let Some((start, end)) = matcher.is_match(line) {
                let text = truncate_utf8(line, request.max_line_bytes as usize).to_owned();
                let line_truncated = text.len() < line.len();
                let before_start = index.saturating_sub(request.context_before as usize);
                let after_end = (index + 1 + request.context_after as usize).min(lines.len());
                items.push(SearchItem::Match {
                    value: TextMatch {
                        path: spec.clone(),
                        line: index as u64 + 1,
                        byte_offset,
                        text,
                        before: lines[before_start..index]
                            .iter()
                            .map(|line| {
                                truncate_utf8(line, request.max_line_bytes as usize).to_owned()
                            })
                            .collect(),
                        after: lines[index + 1..after_end]
                            .iter()
                            .map(|line| {
                                truncate_utf8(line, request.max_line_bytes as usize).to_owned()
                            })
                            .collect(),
                        submatches: vec![TextSubmatch {
                            start: start as u64,
                            end: end as u64,
                            text: line.get(start..end).unwrap_or_default().to_owned(),
                        }],
                        line_truncated,
                    },
                });
            }
            byte_offset += line.len() as u64 + 1;
        }
    }
    Ok(items)
}

enum SearchMatcher {
    Literal(String),
    Regex(Regex),
    Glob(globset::GlobMatcher),
}

impl SearchMatcher {
    fn new(request: &SearchRequest) -> ExecutionResult<Self> {
        match request.syntax {
            SearchSyntax::Literal => Ok(Self::Literal(request.pattern.clone())),
            SearchSyntax::Regex => {
                Regex::new(&request.pattern)
                    .map(Self::Regex)
                    .map_err(|source| {
                        error(
                            ExecutionErrorCode::InvalidRequest,
                            format!("invalid regular expression: {source}"),
                        )
                    })
            }
            SearchSyntax::Glob => Glob::new(&request.pattern)
                .map(|glob| Self::Glob(glob.compile_matcher()))
                .map_err(|source| {
                    error(
                        ExecutionErrorCode::InvalidRequest,
                        format!("invalid glob pattern: {source}"),
                    )
                }),
        }
    }

    fn is_match(&self, value: &str) -> Option<(usize, usize)> {
        match self {
            Self::Literal(pattern) => value
                .find(pattern)
                .map(|start| (start, start + pattern.len())),
            Self::Regex(regex) => regex.find(value).map(|value| (value.start(), value.end())),
            Self::Glob(glob) => glob.is_match(value).then_some((0, value.len())),
        }
    }
}

fn build_glob_set(patterns: &[String]) -> ExecutionResult<Option<GlobSet>> {
    if patterns.is_empty() {
        return Ok(None);
    }
    let mut builder = GlobSetBuilder::new();
    for pattern in patterns {
        builder.add(Glob::new(pattern).map_err(|source| {
            error(
                ExecutionErrorCode::InvalidRequest,
                format!("invalid include/exclude glob: {source}"),
            )
        })?);
    }
    builder.build().map(Some).map_err(|source| {
        error(
            ExecutionErrorCode::InvalidRequest,
            format!("invalid include/exclude glob set: {source}"),
        )
    })
}

fn requested_line(page: &TextPageRequest) -> ExecutionResult<u64> {
    if let Some(cursor) = &page.cursor {
        return cursor
            .as_str()
            .strip_prefix("line:")
            .and_then(|line| line.parse().ok())
            .ok_or_else(|| error(ExecutionErrorCode::InvalidRequest, "invalid read cursor"));
    }
    Ok(page.start_line.unwrap_or(1))
}

fn line_cursor(line: u64) -> ContinuationCursor {
    ContinuationCursor::new(format!("line:{line}")).expect("line cursor is non-empty")
}

fn search_cursor(index: usize) -> ContinuationCursor {
    ContinuationCursor::new(format!("search:{index}")).expect("search cursor is non-empty")
}

fn decode_search_cursor(cursor: Option<&ContinuationCursor>) -> ExecutionResult<usize> {
    let Some(cursor) = cursor else {
        return Ok(0);
    };
    cursor
        .as_str()
        .strip_prefix("search:")
        .and_then(|value| value.parse().ok())
        .ok_or_else(|| error(ExecutionErrorCode::InvalidRequest, "invalid search cursor"))
}

fn truncate_utf8(value: &str, max_bytes: usize) -> &str {
    if value.len() <= max_bytes {
        return value;
    }
    let mut end = max_bytes.min(value.len());
    while !value.is_char_boundary(end) {
        end = end.saturating_sub(1);
    }
    &value[..end]
}

fn estimated_item_bytes(item: &SearchItem) -> u64 {
    match item {
        SearchItem::Path { entry } => entry.name.len() as u64,
        SearchItem::Match { value } => {
            (value.text.len()
                + value.before.iter().map(String::len).sum::<usize>()
                + value.after.iter().map(String::len).sum::<usize>()) as u64
        }
    }
}

fn timestamp(value: std::time::SystemTime) -> Option<execution_contracts::TimestampMs> {
    value
        .duration_since(std::time::UNIX_EPOCH)
        .ok()
        .and_then(|duration| u64::try_from(duration.as_millis()).ok())
        .map(execution_contracts::TimestampMs)
}

fn invalid_request(
    error_value: execution_contracts::ValidationError,
) -> execution_contracts::ExecutionError {
    error(
        ExecutionErrorCode::InvalidRequest,
        format!("invalid request: {error_value}"),
    )
}

#[allow(dead_code)]
fn _bounded_context_window(lines: &[String], limit: usize) -> VecDeque<String> {
    lines.iter().rev().take(limit).cloned().collect()
}
