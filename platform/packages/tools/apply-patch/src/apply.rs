use execution_core::{
    BinaryData, ExecutionError, ExecutionErrorCode as Code, ExecutionResult, OperationContext,
    OperationId, RemovePathRequest, WriteCondition, WriteFileRequest, WriteStrategy,
};
use serde_json::json;

use crate::{
    ApplyPatchOutput, ApplyPatchTool, Hunk, PatchProgress, PreparedOperation, PreparedPatch,
    RunningPatch, error, filesystem, update,
};

pub(crate) fn validate(tool: &ApplyPatchTool<'_>, prepared: &PreparedPatch) -> ExecutionResult<()> {
    if prepared.version != 2
        || prepared.hunks.is_empty()
        || prepared.changes.len() != prepared.hunks.len()
    {
        return Err(error(
            Code::InvalidRequest,
            "invalid or unsupported prepared patch state",
        ));
    }
    if prepared.host_id != tool.runtime.descriptor().host_id {
        return Err(error(
            Code::InvalidRequest,
            "prepared patch belongs to another execution host",
        ));
    }
    if prepared.generation != tool.runtime.descriptor().supervisor_generation_id {
        return Err(error(
            Code::ExecutionLost,
            "patch belongs to an earlier supervisor generation; inspect its outcome before retrying",
        ));
    }
    if prepared.operation_count() > tool.config.max_operations {
        return Err(error(
            Code::ResourceExhausted,
            "prepared patch exceeds this tool's operation limit",
        ));
    }
    Ok(())
}

pub(crate) async fn step(
    tool: &ApplyPatchTool<'_>,
    context: &OperationContext,
    running: &mut RunningPatch,
) -> ExecutionResult<PatchProgress> {
    validate(tool, &running.prepared)?;
    if (running.hunk_index == running.prepared.hunks.len() && !running.pending.is_empty())
        || running.hunk_index > running.prepared.hunks.len()
        || (running.pending.is_empty() && running.next_operation != 0)
        || (!running.pending.is_empty() && running.next_operation >= running.pending.len())
    {
        return Err(error(Code::InvalidRequest, "invalid patch progress"));
    }
    context.checkpoint()?;
    if running.is_complete() {
        return Ok(PatchProgress::Complete(ApplyPatchOutput {
            host_id: running.prepared.host_id.clone(),
            changes: running.prepared.changes.clone(),
        }));
    }
    let result = if running.pending.is_empty() {
        stage_hunk(tool, context, running).await
    } else {
        dispatch(tool, context, &running.pending[running.next_operation])
            .await
            .map(|()| {
                running.applied_operations += 1;
                running.next_operation += 1;
                if running.next_operation == running.pending.len() {
                    running.hunk_index += 1;
                    running.next_operation = 0;
                    running.pending.clear();
                }
            })
    };
    result.map_err(|failure| {
        annotate(
            failure,
            running.applied_operations,
            running.prepared.operation_count(),
        )
    })?;
    // Always return a checkpoint boundary after staging or acknowledging a
    // mutation. Never derive the next hunk before this progress can be saved.
    Ok(PatchProgress::CheckpointRequired)
}

async fn stage_hunk(
    tool: &ApplyPatchTool<'_>,
    context: &OperationContext,
    running: &mut RunningPatch,
) -> ExecutionResult<()> {
    let prepared = &running.prepared;
    let hunk = &prepared.hunks[running.hunk_index];
    let mut operations = Vec::new();
    let content = match &hunk.hunk {
        Hunk::AddFile { contents, .. } => Some(contents.clone()),
        Hunk::DeleteFile { .. } => None,
        Hunk::UpdateFile { path, chunks, .. } => {
            let (original, _) = filesystem::read_text(
                tool.runtime,
                context,
                &hunk.source,
                tool.config.max_file_bytes,
                tool.config.chunk_bytes,
            )
            .await?;
            Some(update::derive_new_contents(
                path,
                &original,
                chunks,
                prepared.update_file_mode,
            )?)
        }
    };
    if let Some(content) = content {
        let limit = tool.config.max_file_bytes.min(
            tool.runtime
                .descriptor()
                .limits
                .max_write_bytes
                .unwrap_or(u64::MAX),
        );
        if content.len() as u64 > limit {
            return Err(error(
                Code::ResourceExhausted,
                format!("patched file exceeds write limit of {limit} bytes"),
            ));
        }
        operations.push(PreparedOperation::Write {
            request: WriteFileRequest {
                expected_generation: Some(prepared.generation.clone()),
                strategy: WriteStrategy::InPlace,
                operation_id: derived_id(&prepared.operation_id, running.applied_operations)?,
                path: hunk.destination.as_ref().unwrap_or(&hunk.source).clone(),
                data: BinaryData::new(content.into_bytes()),
                condition: WriteCondition::Any,
                // Missing-parent retry applies to add/move, not ordinary update.
                create_parents: matches!(hunk.hunk, Hunk::AddFile { .. })
                    || hunk.destination.is_some(),
                follow_symlinks: true,
            },
        });
    }
    if matches!(hunk.hunk, Hunk::DeleteFile { .. }) || hunk.destination.is_some() {
        operations.push(PreparedOperation::Remove {
            request: RemovePathRequest {
                target_kind: execution_core::RemoveTargetKind::File,
                expected_generation: Some(prepared.generation.clone()),
                operation_id: derived_id(
                    &prepared.operation_id,
                    running.applied_operations + operations.len(),
                )?,
                path: hunk.source.clone(),
                recursive: false,
                ignore_missing: false,
                expected_revision: None,
            },
        });
    }
    running.pending = operations;
    Ok(())
}

async fn dispatch(
    tool: &ApplyPatchTool<'_>,
    context: &OperationContext,
    operation: &PreparedOperation,
) -> ExecutionResult<()> {
    match operation {
        PreparedOperation::Write { request } => {
            tool.validate_target(&request.path)?;
            let reply = tool
                .runtime
                .filesystem()
                .write(context, request.clone())
                .await?;
            if reply.path != request.path || reply.bytes_written != request.data.len() as u64 {
                return Err(error(
                    Code::Internal,
                    "host returned an inconsistent patch write result; outcome is uncertain",
                ));
            }
        }
        PreparedOperation::Remove { request } => {
            tool.validate_target(&request.path)?;
            // Codex refuses directory targets, including directory symlinks.
            // Do not stat here: replay after an acknowledged unlink must reach
            // the supervisor's receipt before checking the current filesystem.
            let reply = tool
                .runtime
                .filesystem()
                .remove(context, request.clone())
                .await?;
            if !reply.removed {
                return Err(error(
                    Code::Internal,
                    "host reported that a required patch target was not removed",
                ));
            }
        }
    }
    Ok(())
}

fn derived_id(base: &OperationId, index: usize) -> ExecutionResult<OperationId> {
    OperationId::new(format!("{}:apply_patch:{index}", base.as_str())).map_err(Into::into)
}

fn annotate(mut failure: ExecutionError, applied: usize, total: usize) -> ExecutionError {
    failure
        .details
        .entry("source".to_string())
        .or_insert_with(|| json!("tool-apply-patch"));
    failure
        .details
        .insert("applied_operations".to_string(), json!(applied));
    failure
        .details
        .insert("total_operations".to_string(), json!(total));
    failure
}
