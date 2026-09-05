use crate::{EditConfig, ObservedFile, error};
use execution_core::{
    ExecutionErrorCode as Code, ExecutionResult, ExecutionRuntime, FileKind, OperationContext,
    ReadFileRequest, StatRequest,
};

pub(crate) async fn read(
    runtime: &dyn ExecutionRuntime,
    context: &OperationContext,
    observed: &ObservedFile,
    config: &EditConfig,
) -> ExecutionResult<String> {
    let metadata = runtime
        .filesystem()
        .stat(
            context,
            StatRequest {
                path: observed.path.clone(),
                follow_symlinks: true,
            },
        )
        .await?;
    context.checkpoint()?;
    if metadata.path != observed.path {
        return Err(error(
            Code::Internal,
            "host returned metadata for a different path",
        ));
    }
    if metadata.kind != FileKind::File {
        return Err(error(
            if metadata.kind == FileKind::Directory {
                Code::IsDirectory
            } else {
                Code::Unsupported
            },
            "edit requires a regular UTF-8 text file",
        ));
    }
    if metadata.revision.as_ref() != Some(&observed.revision) {
        return Err(error(
            Code::RevisionConflict,
            "file changed since it was observed; read it again before editing",
        ));
    }
    if metadata.size > config.max_file_bytes {
        return Err(error(
            Code::ResourceExhausted,
            "file exceeds the configured edit size limit",
        ));
    }
    let chunk = config.chunk_bytes.min(
        runtime
            .descriptor()
            .limits
            .max_read_bytes
            .unwrap_or(u64::MAX),
    );
    if chunk == 0 {
        return Err(error(
            Code::Unsupported,
            "host advertised a zero read limit",
        ));
    }
    let mut bytes = Vec::new();
    loop {
        context.checkpoint()?;
        let offset = bytes.len() as u64;
        // Empty files still get one byte-read to validate the observed revision.
        let max_bytes = chunk.min(metadata.size.saturating_sub(offset).max(1));
        let reply = runtime
            .filesystem()
            .read(
                context,
                ReadFileRequest {
                    path: observed.path.clone(),
                    offset,
                    max_bytes,
                    follow_symlinks: true,
                },
            )
            .await?;
        context.checkpoint()?;
        if reply.metadata.revision.as_ref() != Some(&observed.revision)
            || reply.metadata.size != metadata.size
        {
            return Err(error(
                Code::RevisionConflict,
                "file changed while preparing the edit; read it again",
            ));
        }
        if reply.offset != offset
            || reply.metadata.path != observed.path
            || reply.metadata.kind != FileKind::File
            || reply.data.len() as u64 > max_bytes
            || (reply.data.is_empty() && !reply.eof)
        {
            return Err(error(
                Code::Internal,
                "host returned an invalid file-read response",
            ));
        }
        let end = offset
            .checked_add(reply.data.len() as u64)
            .ok_or_else(|| error(Code::Internal, "file offset overflow"))?;
        if end > metadata.size || reply.eof != (end == metadata.size) {
            return Err(error(
                Code::Internal,
                "host returned an inconsistent file-read EOF",
            ));
        }
        if reply.data.as_slice().contains(&0) {
            return Err(error(
                Code::Unsupported,
                "file contains binary data (NUL); edit supports UTF-8 text only",
            ));
        }
        bytes.extend_from_slice(reply.data.as_slice());
        if reply.eof {
            break;
        }
    }
    String::from_utf8(bytes).map_err(|_| error(Code::Unsupported, "file is not valid UTF-8 text"))
}
