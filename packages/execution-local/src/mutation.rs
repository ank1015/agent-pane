use std::{collections::HashMap, path::PathBuf, sync::Arc};

use async_trait::async_trait;
use execution_contracts::{
    AbortMutationRequest, AppliedChange, ApplyMutationRequest, CommitMutationRequest,
    ContentSource, ExecutionErrorCode, FileChangeKind, FileRevision, MutationOperation,
    MutationPlan, MutationPreview, MutationResult, MutationStatus, OperationId, PathSpec,
    PrepareMutationRequest, PreparedMutation, PreparedMutationId, TimestampMs, Validate,
    WriteBytesRequest, WriteCondition,
};
use execution_runtime::{BasicFileSystem, ExecutionResult, OperationContext, WorkspaceMutation};
use tokio::sync::Mutex;
use uuid::Uuid;

use crate::{
    error::{LocalExecutionError, error, io_error},
    filesystem::LocalFileSystem,
    path_resolver::PathResolver,
};

const PREPARED_LIFETIME_MS: u64 = 5 * 60 * 1_000;
const MAX_INLINE_DIFF_BYTES: usize = 64 * 1024;

pub(crate) struct LocalWorkspaceMutation {
    resolver: Arc<PathResolver>,
    filesystem: LocalFileSystem,
    prepared: Mutex<HashMap<PreparedMutationId, PreparedRecord>>,
    completed: Mutex<HashMap<OperationId, MutationResult>>,
}

#[derive(Clone)]
struct PreparedRecord {
    contract: PreparedMutation,
    plan: MutationPlan,
    expected: Vec<ExpectedPath>,
}

#[derive(Clone)]
struct ExpectedPath {
    path: PathSpec,
    revision: Option<FileRevision>,
    existed: bool,
}

impl LocalWorkspaceMutation {
    pub async fn new(
        resolver: Arc<PathResolver>,
        filesystem: LocalFileSystem,
        staging_directory: PathBuf,
    ) -> Result<Self, LocalExecutionError> {
        tokio::fs::create_dir_all(staging_directory).await?;
        Ok(Self {
            resolver,
            filesystem,
            prepared: Mutex::new(HashMap::new()),
            completed: Mutex::new(HashMap::new()),
        })
    }

    async fn prepare_plan(&self, plan: MutationPlan) -> ExecutionResult<PreparedMutation> {
        plan.validate().map_err(invalid_request)?;
        reject_duplicate_targets(&plan)?;
        let mut expected = Vec::new();
        let mut previews = Vec::new();
        for (index, operation) in plan.operations.iter().enumerate() {
            let (mut operation_expected, preview) =
                self.preview_operation(index, operation).await?;
            expected.append(&mut operation_expected);
            previews.push(preview);
        }
        let prepared_id = PreparedMutationId::new(Uuid::now_v7().to_string())
            .expect("UUID prepared mutation identifier is valid");
        let contract = PreparedMutation {
            prepared_id: prepared_id.clone(),
            expires_at: TimestampMs(now_ms().saturating_add(PREPARED_LIFETIME_MS)),
            previews,
            atomic_commit_supported: false,
        };
        self.prepared.lock().await.insert(
            prepared_id,
            PreparedRecord {
                contract: contract.clone(),
                plan,
                expected,
            },
        );
        Ok(contract)
    }

    async fn preview_operation(
        &self,
        index: usize,
        operation: &MutationOperation,
    ) -> ExecutionResult<(Vec<ExpectedPath>, MutationPreview)> {
        match operation {
            MutationOperation::PutFile { path, content, .. } => {
                let old = self.read_optional(path).await?;
                let new = self.filesystem.content_bytes(content).await?;
                Ok((
                    vec![expected(path.clone(), old.as_ref())],
                    preview(
                        index,
                        path.clone(),
                        change_for(old.is_some()),
                        old.as_deref(),
                        Some(&new),
                    ),
                ))
            }
            MutationOperation::CreateFile { path, content, .. } => {
                let old = self.read_optional(path).await?;
                if old.is_some() {
                    return Err(error(
                        ExecutionErrorCode::AlreadyExists,
                        "create target already exists",
                    ));
                }
                let new = self.filesystem.content_bytes(content).await?;
                Ok((
                    vec![expected(path.clone(), None)],
                    preview(
                        index,
                        path.clone(),
                        FileChangeKind::Created,
                        None,
                        Some(&new),
                    ),
                ))
            }
            MutationOperation::ReplaceText {
                path, replacements, ..
            } => {
                let old = self.read_required(path).await?;
                let old_text = String::from_utf8(old.clone()).map_err(|_| {
                    error(
                        ExecutionErrorCode::InvalidRequest,
                        "replace_text requires UTF-8",
                    )
                })?;
                let new_text = apply_replacements(old_text, replacements)?;
                Ok((
                    vec![expected(path.clone(), Some(&old))],
                    preview(
                        index,
                        path.clone(),
                        FileChangeKind::Modified,
                        Some(&old),
                        Some(new_text.as_bytes()),
                    ),
                ))
            }
            MutationOperation::ApplyTextPatch {
                path,
                hunks,
                match_policy,
                ..
            } => {
                let old = self.read_required(path).await?;
                let old_text = String::from_utf8(old.clone()).map_err(|_| {
                    error(
                        ExecutionErrorCode::InvalidRequest,
                        "text patch requires UTF-8",
                    )
                })?;
                let new_text = apply_hunks(old_text, hunks, *match_policy)?;
                Ok((
                    vec![expected(path.clone(), Some(&old))],
                    preview(
                        index,
                        path.clone(),
                        FileChangeKind::Modified,
                        Some(&old),
                        Some(new_text.as_bytes()),
                    ),
                ))
            }
            MutationOperation::Remove { path, .. } => {
                let old = self.read_optional(path).await?;
                if old.is_none() {
                    self.filesystem.metadata_for(path, false).await?;
                }
                Ok((
                    vec![expected(path.clone(), old.as_ref())],
                    preview(
                        index,
                        path.clone(),
                        FileChangeKind::Removed,
                        old.as_deref(),
                        None,
                    ),
                ))
            }
            MutationOperation::Move {
                source,
                destination,
                ..
            } => {
                self.filesystem.metadata_for(source, false).await?;
                let source_bytes = self.read_optional(source).await?;
                let destination_bytes = self.read_optional(destination).await?;
                Ok((
                    vec![
                        expected(source.clone(), source_bytes.as_ref()),
                        expected(destination.clone(), destination_bytes.as_ref()),
                    ],
                    preview(
                        index,
                        destination.clone(),
                        FileChangeKind::Moved,
                        destination_bytes.as_deref(),
                        source_bytes.as_deref(),
                    ),
                ))
            }
            MutationOperation::Copy {
                source,
                destination,
                ..
            } => {
                self.filesystem.metadata_for(source, false).await?;
                let source_bytes = self.read_optional(source).await?;
                let destination_bytes = self.read_optional(destination).await?;
                Ok((
                    vec![
                        expected(source.clone(), source_bytes.as_ref()),
                        expected(destination.clone(), destination_bytes.as_ref()),
                    ],
                    preview(
                        index,
                        destination.clone(),
                        FileChangeKind::Copied,
                        destination_bytes.as_deref(),
                        source_bytes.as_deref(),
                    ),
                ))
            }
        }
    }

    async fn verify_expected(&self, expected: &[ExpectedPath]) -> ExecutionResult<()> {
        for state in expected {
            let current = self.read_optional(&state.path).await?;
            if current.is_some() != state.existed {
                return Err(error(
                    ExecutionErrorCode::StaleRevision,
                    "a mutation target changed after preparation",
                ));
            }
            if let Some(expected_revision) = &state.revision {
                let Some(current) = current else {
                    return Err(error(
                        ExecutionErrorCode::StaleRevision,
                        "a mutation target was removed after preparation",
                    ));
                };
                let revision = FileRevision::new(crate::artifacts::hex_digest(&current))
                    .expect("SHA-256 digest is non-empty");
                if revision != *expected_revision {
                    return Err(error(
                        ExecutionErrorCode::StaleRevision,
                        "a mutation target changed after preparation",
                    ));
                }
            }
        }
        Ok(())
    }

    async fn execute_plan(
        &self,
        context: &OperationContext,
        operation_id: OperationId,
        plan: MutationPlan,
    ) -> ExecutionResult<MutationResult> {
        if let Some(result) = self.completed.lock().await.get(&operation_id).cloned() {
            return Ok(result);
        }
        let mut changes = Vec::new();
        for (index, operation) in plan.operations.into_iter().enumerate() {
            if context.is_cancelled() {
                return Err(error(
                    ExecutionErrorCode::Cancelled,
                    "mutation was cancelled",
                ));
            }
            let (path, change, revision) = self.execute_operation(context, operation).await?;
            changes.push(AppliedChange {
                operation_index: index as u32,
                path,
                change,
                revision,
                diff: None,
            });
        }
        let result = MutationResult {
            operation_id: operation_id.clone(),
            status: MutationStatus::Committed,
            changes,
            atomic: false,
            diagnostics: None,
        };
        self.completed
            .lock()
            .await
            .insert(operation_id, result.clone());
        Ok(result)
    }

    async fn execute_operation(
        &self,
        context: &OperationContext,
        operation: MutationOperation,
    ) -> ExecutionResult<(PathSpec, FileChangeKind, Option<FileRevision>)> {
        match operation {
            MutationOperation::PutFile {
                path,
                content,
                create_parents,
                expected_revision,
                ..
            } => {
                let result = self
                    .filesystem
                    .write_bytes(
                        context,
                        WriteBytesRequest {
                            path: path.clone(),
                            content,
                            condition: expected_revision.map_or(WriteCondition::Any, |revision| {
                                WriteCondition::MatchRevision { revision }
                            }),
                            create_parents,
                            atomic_replace: true,
                            follow_symlinks: true,
                        },
                    )
                    .await?;
                Ok((path, change_for(result.existed), Some(result.revision)))
            }
            MutationOperation::CreateFile {
                path,
                content,
                create_parents,
            } => {
                let result = self
                    .filesystem
                    .write_bytes(
                        context,
                        WriteBytesRequest {
                            path: path.clone(),
                            content,
                            condition: WriteCondition::MustNotExist,
                            create_parents,
                            atomic_replace: true,
                            follow_symlinks: false,
                        },
                    )
                    .await?;
                Ok((path, FileChangeKind::Created, Some(result.revision)))
            }
            MutationOperation::ReplaceText {
                path,
                replacements,
                expected_revision,
                ..
            } => {
                let old = self.read_required(&path).await?;
                let old_text = String::from_utf8(old).map_err(|_| {
                    error(
                        ExecutionErrorCode::InvalidRequest,
                        "replace_text requires UTF-8",
                    )
                })?;
                let new = apply_replacements(old_text, &replacements)?;
                let condition = expected_revision.map_or(WriteCondition::Any, |revision| {
                    WriteCondition::MatchRevision { revision }
                });
                let result = self
                    .filesystem
                    .write_bytes(
                        context,
                        WriteBytesRequest {
                            path: path.clone(),
                            content: ContentSource::Text { content: new },
                            condition,
                            create_parents: false,
                            atomic_replace: true,
                            follow_symlinks: true,
                        },
                    )
                    .await?;
                Ok((path, FileChangeKind::Modified, Some(result.revision)))
            }
            MutationOperation::ApplyTextPatch {
                path,
                hunks,
                match_policy,
                expected_revision,
                ..
            } => {
                let old = self.read_required(&path).await?;
                let old_text = String::from_utf8(old).map_err(|_| {
                    error(
                        ExecutionErrorCode::InvalidRequest,
                        "text patch requires UTF-8",
                    )
                })?;
                let new = apply_hunks(old_text, &hunks, match_policy)?;
                let condition = expected_revision.map_or(WriteCondition::Any, |revision| {
                    WriteCondition::MatchRevision { revision }
                });
                let result = self
                    .filesystem
                    .write_bytes(
                        context,
                        WriteBytesRequest {
                            path: path.clone(),
                            content: ContentSource::Text { content: new },
                            condition,
                            create_parents: false,
                            atomic_replace: true,
                            follow_symlinks: true,
                        },
                    )
                    .await?;
                Ok((path, FileChangeKind::Modified, Some(result.revision)))
            }
            MutationOperation::Remove {
                path,
                recursive,
                force,
                expected_revision,
            } => {
                self.filesystem
                    .remove(
                        context,
                        execution_contracts::RemovePathRequest {
                            path: path.clone(),
                            recursive,
                            force,
                            expected_revision,
                        },
                    )
                    .await?;
                Ok((path, FileChangeKind::Removed, None))
            }
            MutationOperation::Move {
                source,
                destination,
                overwrite,
                expected_source_revision,
            } => {
                self.filesystem
                    .move_path(
                        context,
                        execution_contracts::MovePathRequest {
                            source,
                            destination: destination.clone(),
                            overwrite,
                            expected_source_revision,
                        },
                    )
                    .await?;
                Ok((destination, FileChangeKind::Moved, None))
            }
            MutationOperation::Copy {
                source,
                destination,
                recursive,
                overwrite,
            } => {
                self.filesystem
                    .copy_path(
                        context,
                        execution_contracts::CopyPathRequest {
                            source,
                            destination: destination.clone(),
                            recursive,
                            overwrite,
                        },
                    )
                    .await?;
                Ok((destination, FileChangeKind::Copied, None))
            }
        }
    }

    async fn read_required(&self, path: &PathSpec) -> ExecutionResult<Vec<u8>> {
        let resolved = self.resolver.resolve_existing(path, true).await?;
        tokio::fs::read(&resolved.path)
            .await
            .map_err(|source| io_error(&resolved.path, source))
    }

    async fn read_optional(&self, path: &PathSpec) -> ExecutionResult<Option<Vec<u8>>> {
        match self.resolver.resolve_existing(path, true).await {
            Ok(resolved) => {
                let metadata = tokio::fs::metadata(&resolved.path)
                    .await
                    .map_err(|source| io_error(&resolved.path, source))?;
                if metadata.is_file() {
                    tokio::fs::read(&resolved.path)
                        .await
                        .map(Some)
                        .map_err(|source| io_error(&resolved.path, source))
                } else {
                    Ok(None)
                }
            }
            Err(source) if source.code == ExecutionErrorCode::NotFound => Ok(None),
            Err(source) => Err(source),
        }
    }
}

#[async_trait]
impl WorkspaceMutation for LocalWorkspaceMutation {
    async fn prepare(
        &self,
        _context: &OperationContext,
        request: PrepareMutationRequest,
    ) -> ExecutionResult<PreparedMutation> {
        self.prepare_plan(request.plan).await
    }

    async fn commit(
        &self,
        context: &OperationContext,
        request: CommitMutationRequest,
    ) -> ExecutionResult<MutationResult> {
        if let Some(result) = self
            .completed
            .lock()
            .await
            .get(&request.operation_id)
            .cloned()
        {
            return Ok(result);
        }
        let record = self
            .prepared
            .lock()
            .await
            .get(&request.prepared_id)
            .cloned()
            .ok_or_else(|| {
                error(
                    ExecutionErrorCode::NotFound,
                    "prepared mutation was not found",
                )
            })?;
        if record.contract.expires_at.0 < now_ms() {
            self.prepared.lock().await.remove(&request.prepared_id);
            return Err(error(
                ExecutionErrorCode::DeadlineExceeded,
                "prepared mutation expired",
            ));
        }
        self.verify_expected(&record.expected).await?;
        let result = self
            .execute_plan(context, request.operation_id, record.plan)
            .await?;
        self.prepared.lock().await.remove(&request.prepared_id);
        Ok(result)
    }

    async fn abort(
        &self,
        _context: &OperationContext,
        request: AbortMutationRequest,
    ) -> ExecutionResult<()> {
        self.prepared.lock().await.remove(&request.prepared_id);
        Ok(())
    }

    async fn apply(
        &self,
        context: &OperationContext,
        request: ApplyMutationRequest,
    ) -> ExecutionResult<MutationResult> {
        if let Some(result) = self
            .completed
            .lock()
            .await
            .get(&request.operation_id)
            .cloned()
        {
            return Ok(result);
        }
        let prepared = self.prepare_plan(request.plan).await?;
        self.commit(
            context,
            CommitMutationRequest {
                prepared_id: prepared.prepared_id,
                operation_id: request.operation_id,
            },
        )
        .await
    }
}

fn expected(path: PathSpec, bytes: Option<&Vec<u8>>) -> ExpectedPath {
    ExpectedPath {
        path,
        revision: bytes.map(|bytes| {
            FileRevision::new(crate::artifacts::hex_digest(bytes))
                .expect("SHA-256 digest is non-empty")
        }),
        existed: bytes.is_some(),
    }
}

fn change_for(existed: bool) -> FileChangeKind {
    if existed {
        FileChangeKind::Modified
    } else {
        FileChangeKind::Created
    }
}

fn preview(
    index: usize,
    path: PathSpec,
    change: FileChangeKind,
    old: Option<&[u8]>,
    new: Option<&[u8]>,
) -> MutationPreview {
    let mut diff = String::new();
    if let Some(old) = old.and_then(|bytes| std::str::from_utf8(bytes).ok()) {
        diff.push_str("--- before\n");
        diff.push_str(old);
        if !old.ends_with('\n') {
            diff.push('\n');
        }
    }
    if let Some(new) = new.and_then(|bytes| std::str::from_utf8(bytes).ok()) {
        diff.push_str("+++ after\n");
        diff.push_str(new);
    }
    if diff.len() > MAX_INLINE_DIFF_BYTES {
        diff.truncate(floor_char_boundary(&diff, MAX_INLINE_DIFF_BYTES));
        diff.push_str("\n... diff truncated ...\n");
    }
    MutationPreview {
        operation_index: index as u32,
        path,
        change,
        diff,
        base_revision: old.map(|bytes| {
            FileRevision::new(crate::artifacts::hex_digest(bytes))
                .expect("SHA-256 digest is non-empty")
        }),
    }
}

fn apply_replacements(
    mut content: String,
    replacements: &[execution_contracts::TextReplacement],
) -> ExecutionResult<String> {
    for replacement in replacements {
        let count = content.matches(&replacement.old_text).count();
        match replacement.occurrence {
            execution_contracts::OccurrencePolicy::Unique if count != 1 => {
                return Err(error(
                    ExecutionErrorCode::Conflict,
                    format!("expected one replacement match, found {count}"),
                ));
            }
            execution_contracts::OccurrencePolicy::First
            | execution_contracts::OccurrencePolicy::All
                if count == 0 =>
            {
                return Err(error(
                    ExecutionErrorCode::Conflict,
                    "replacement text was not found",
                ));
            }
            _ => {}
        }
        content = match replacement.occurrence {
            execution_contracts::OccurrencePolicy::Unique
            | execution_contracts::OccurrencePolicy::First => {
                content.replacen(&replacement.old_text, &replacement.new_text, 1)
            }
            execution_contracts::OccurrencePolicy::All => {
                content.replace(&replacement.old_text, &replacement.new_text)
            }
        };
    }
    Ok(content)
}

fn apply_hunks(
    content: String,
    hunks: &[execution_contracts::TextPatchHunk],
    policy: execution_contracts::PatchMatchPolicy,
) -> ExecutionResult<String> {
    let mut lines: Vec<String> = content.lines().map(ToOwned::to_owned).collect();
    let trailing_newline = content.ends_with('\n');
    for hunk in hunks {
        let position = find_hunk(&lines, &hunk.old_lines, policy).ok_or_else(|| {
            error(
                ExecutionErrorCode::Conflict,
                "patch hunk did not match the current file",
            )
        })?;
        lines.splice(
            position..position + hunk.old_lines.len(),
            hunk.new_lines.clone(),
        );
    }
    let mut result = lines.join("\n");
    if trailing_newline && !result.ends_with('\n') {
        result.push('\n');
    }
    Ok(result)
}

fn find_hunk(
    lines: &[String],
    old_lines: &[String],
    policy: execution_contracts::PatchMatchPolicy,
) -> Option<usize> {
    if old_lines.is_empty() {
        return Some(lines.len());
    }
    lines.windows(old_lines.len()).position(|window| {
        window
            .iter()
            .zip(old_lines)
            .all(|(actual, expected)| match policy {
                execution_contracts::PatchMatchPolicy::Exact
                | execution_contracts::PatchMatchPolicy::Context => actual == expected,
                execution_contracts::PatchMatchPolicy::WhitespaceTolerant => {
                    actual.trim() == expected.trim()
                }
            })
    })
}

fn reject_duplicate_targets(plan: &MutationPlan) -> ExecutionResult<()> {
    let mut targets = Vec::new();
    for operation in &plan.operations {
        let target = match operation {
            MutationOperation::PutFile { path, .. }
            | MutationOperation::CreateFile { path, .. }
            | MutationOperation::ReplaceText { path, .. }
            | MutationOperation::ApplyTextPatch { path, .. }
            | MutationOperation::Remove { path, .. } => path,
            MutationOperation::Move { destination, .. }
            | MutationOperation::Copy { destination, .. } => destination,
        };
        if targets.contains(target) {
            return Err(error(
                ExecutionErrorCode::Unsupported,
                "the initial local mutation engine does not accept duplicate target paths",
            ));
        }
        targets.push(target.clone());
    }
    Ok(())
}

fn invalid_request(
    source: execution_contracts::ValidationError,
) -> execution_contracts::ExecutionError {
    error(
        ExecutionErrorCode::InvalidRequest,
        format!("invalid mutation plan: {source}"),
    )
}

fn now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .ok()
        .and_then(|duration| u64::try_from(duration.as_millis()).ok())
        .unwrap_or(0)
}

fn floor_char_boundary(value: &str, index: usize) -> usize {
    let mut index = index.min(value.len());
    while !value.is_char_boundary(index) {
        index = index.saturating_sub(1);
    }
    index
}
