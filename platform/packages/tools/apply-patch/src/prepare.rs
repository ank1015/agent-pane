use std::collections::HashSet;

use execution_core::{ExecutionErrorCode as Code, ExecutionResult, OperationContext};

use crate::{
    AppliedPatchChange, AppliedPatchChangeKind, ApplyPatchState, ApplyPatchTool, Hunk,
    PreparedHunk, PreparedPatch, error, filesystem, parser, update,
};

pub(crate) async fn prepare(
    tool: &ApplyPatchTool<'_>,
    context: &OperationContext,
    patch: &str,
    state: ApplyPatchState,
) -> ExecutionResult<PreparedPatch> {
    context.checkpoint()?;
    if patch.len() as u64 > tool.config.max_patch_bytes {
        return Err(error(
            Code::ResourceExhausted,
            "patch exceeds the configured input limit",
        ));
    }
    let parsed = parser::parse_patch(patch).map_err(|failure| {
        error(
            Code::InvalidRequest,
            format!("apply_patch verification failed: {failure}"),
        )
    })?;
    if parsed.environment_id.is_some() {
        return Err(error(
            Code::InvalidRequest,
            "select the execution environment before calling this host-bound apply_patch tool",
        ));
    }
    if parsed.hunks.is_empty() {
        return Err(error(Code::InvalidRequest, "No files were modified."));
    }
    let mut hunks = Vec::new();
    let mut changes = Vec::new();
    let mut sources = HashSet::new();
    let mut count = 0usize;
    for hunk in parsed.hunks {
        context.checkpoint()?;
        let source = tool.resolve_path(hunk.source_path())?;
        if !sources.insert(source.clone()) {
            return Err(error(
                Code::InvalidRequest,
                format!(
                    "apply_patch verification failed: invalid patch: multiple operations target {}",
                    hunk.source_path()
                ),
            ));
        }
        let (kind, destination) = match &hunk {
            // Codex does not stat add destinations during initial verification.
            Hunk::AddFile { .. } => (AppliedPatchChangeKind::Add, None),
            Hunk::DeleteFile { .. } => {
                filesystem::read_text(
                    tool.runtime,
                    context,
                    &source,
                    tool.config.max_file_bytes,
                    tool.config.chunk_bytes,
                )
                .await?;
                (AppliedPatchChangeKind::Delete, None)
            }
            Hunk::UpdateFile {
                path,
                move_path,
                chunks,
            } => {
                let (original, _) = filesystem::read_text(
                    tool.runtime,
                    context,
                    &source,
                    tool.config.max_file_bytes,
                    tool.config.chunk_bytes,
                )
                .await?;
                update::derive_new_contents(path, &original, chunks, tool.config.update_file_mode)?;
                (
                    AppliedPatchChangeKind::Modify,
                    move_path
                        .as_deref()
                        .map(|path| tool.resolve_path(path))
                        .transpose()?,
                )
            }
        };
        count = count
            .checked_add(if destination.is_some() { 2 } else { 1 })
            .filter(|count| *count <= tool.config.max_operations)
            .ok_or_else(|| {
                error(
                    Code::ResourceExhausted,
                    "patch exceeds the configured operation limit",
                )
            })?;
        changes.push(AppliedPatchChange {
            kind,
            path: hunk.affected_path().to_string(),
        });
        hunks.push(PreparedHunk {
            source,
            destination,
            hunk,
        });
    }
    Ok(PreparedPatch {
        version: 2,
        host_id: tool.runtime.descriptor().host_id.clone(),
        generation: tool.runtime.descriptor().supervisor_generation_id.clone(),
        operation_id: state.operation_id,
        update_file_mode: tool.config.update_file_mode,
        hunks,
        changes,
    })
}
