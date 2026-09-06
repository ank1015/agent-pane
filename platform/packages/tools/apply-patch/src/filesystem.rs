use execution_core::{
    ExecutionErrorCode as Code, ExecutionPath, ExecutionResult, ExecutionRuntime, FileKind,
    FileMetadata, OperationContext, ReadFileRequest, StatRequest,
};

use crate::error;

pub(crate) async fn stat_optional(
    runtime: &dyn ExecutionRuntime,
    context: &OperationContext,
    path: &ExecutionPath,
) -> ExecutionResult<Option<FileMetadata>> {
    let metadata = stat_optional_with_follow(runtime, context, path, true).await?;
    if let Some(metadata) = &metadata
        && metadata.kind != FileKind::File
    {
        return Err(error(
            if metadata.kind == FileKind::Directory {
                Code::IsDirectory
            } else {
                Code::Unsupported
            },
            "apply_patch targets must be regular files",
        ));
    }
    Ok(metadata)
}

async fn stat_optional_with_follow(
    runtime: &dyn ExecutionRuntime,
    context: &OperationContext,
    path: &ExecutionPath,
    follow_symlinks: bool,
) -> ExecutionResult<Option<FileMetadata>> {
    match runtime
        .filesystem()
        .stat(
            context,
            StatRequest {
                path: path.clone(),
                follow_symlinks,
            },
        )
        .await
    {
        Ok(metadata) => {
            context.checkpoint()?;
            if metadata.path != *path {
                return Err(error(
                    Code::Internal,
                    "host returned metadata for a different patch path",
                ));
            }
            Ok(Some(metadata))
        }
        Err(failure) if failure.code == Code::NotFound => Ok(None),
        Err(failure) => Err(failure),
    }
}

pub(crate) async fn read_text(
    runtime: &dyn ExecutionRuntime,
    context: &OperationContext,
    path: &ExecutionPath,
    max_file_bytes: u64,
    chunk_bytes: u64,
) -> ExecutionResult<(String, FileMetadata)> {
    let metadata = stat_optional(runtime, context, path)
        .await?
        .ok_or_else(|| {
            error(
                Code::NotFound,
                format!("Failed to read {path:?}: file not found"),
            )
        })?;
    if metadata.size > max_file_bytes {
        return Err(error(
            Code::ResourceExhausted,
            format!(
                "file {} contains {} bytes; apply_patch file limit is {max_file_bytes}",
                path.path, metadata.size
            ),
        ));
    }
    let revision = metadata.revision.as_ref().ok_or_else(|| {
        error(
            Code::Unsupported,
            "host did not provide a file revision for patch verification",
        )
    })?;
    let request_bytes = chunk_bytes.min(
        runtime
            .descriptor()
            .limits
            .max_read_bytes
            .unwrap_or(u64::MAX),
    );
    if request_bytes == 0 {
        return Err(error(
            Code::Unsupported,
            "host advertised a zero read limit",
        ));
    }

    let mut bytes = Vec::new();
    loop {
        context.checkpoint()?;
        let offset = bytes.len() as u64;
        let max_bytes = request_bytes.min(metadata.size.saturating_sub(offset).max(1));
        let reply = runtime
            .filesystem()
            .read(
                context,
                ReadFileRequest {
                    path: path.clone(),
                    offset,
                    max_bytes,
                    follow_symlinks: true,
                },
            )
            .await?;
        context.checkpoint()?;
        if reply.metadata.revision.as_ref() != Some(revision)
            || reply.metadata.size != metadata.size
        {
            return Err(error(
                Code::RevisionConflict,
                "file changed while verifying the patch; retry apply_patch",
            ));
        }
        let data = reply.data.as_slice();
        if reply.offset != offset
            || reply.metadata.path != *path
            || reply.metadata.kind != FileKind::File
            || data.len() as u64 > max_bytes
            || (data.is_empty() && !reply.eof)
        {
            return Err(error(
                Code::Internal,
                "host returned an invalid patch file-read response",
            ));
        }
        let end = offset
            .checked_add(data.len() as u64)
            .ok_or_else(|| error(Code::Internal, "patch file offset overflow"))?;
        if end > metadata.size || reply.eof != (end == metadata.size) {
            return Err(error(
                Code::RevisionConflict,
                "file size changed or host returned an inconsistent EOF",
            ));
        }
        bytes.try_reserve(data.len()).map_err(|_| {
            error(
                Code::ResourceExhausted,
                "insufficient memory to verify the patch target",
            )
        })?;
        bytes.extend_from_slice(data);
        if reply.eof {
            break;
        }
    }
    let content = String::from_utf8(bytes).map_err(|_| {
        error(
            Code::Unsupported,
            "apply_patch supports UTF-8 text files only",
        )
    })?;
    Ok((content, metadata))
}
