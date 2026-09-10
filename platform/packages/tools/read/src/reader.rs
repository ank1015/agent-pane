use crate::{ReadConfig, ReadOutput, error, text::LineReader};
use execution_core::{
    ExecutionErrorCode as Code, ExecutionPath, ExecutionResult, ExecutionRuntime, FileKind,
    OperationContext, ReadFileRequest, StatRequest,
};

pub(crate) async fn read(
    runtime: &dyn ExecutionRuntime,
    context: &OperationContext,
    path: ExecutionPath,
    start: u64,
    limit: u64,
    config: &ReadConfig,
) -> ExecutionResult<ReadOutput> {
    let metadata = runtime
        .filesystem()
        .stat(
            context,
            StatRequest {
                path: path.clone(),
                follow_symlinks: true,
            },
        )
        .await?;
    context.checkpoint()?;
    if metadata.path != path {
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
            "read requires a regular text file; use bash to list directories",
        ));
    }
    let revision = metadata.revision.clone().ok_or_else(|| {
        error(
            Code::Unsupported,
            "host did not provide a file revision for a consistent read",
        )
    })?;
    let mut reader = LineReader::new(start, limit, config)?;
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
    let mut position = 0u64;
    loop {
        context.checkpoint()?;
        let remaining = config.max_scan_bytes.saturating_sub(position);
        if remaining == 0 {
            return Err(error(
                Code::ResourceExhausted,
                "read scan limit reached; narrow the file with bash or increase the harness scan limit",
            ));
        }
        let max_bytes = chunk.min(remaining);
        let reply = runtime
            .filesystem()
            .read(
                context,
                ReadFileRequest {
                    path: path.clone(),
                    offset: position,
                    max_bytes,
                    follow_symlinks: true,
                },
            )
            .await?;
        context.checkpoint()?;
        if reply.metadata.revision.as_ref() != Some(&revision)
            || reply.metadata.size != metadata.size
        {
            return Err(error(
                Code::RevisionConflict,
                "file changed while reading; retry the read",
            ));
        }
        let data = reply.data.as_slice();
        if reply.offset != position
            || reply.metadata.path != path
            || data.len() as u64 > max_bytes
            || reply.metadata.kind != FileKind::File
            || (data.is_empty() && !reply.eof)
        {
            return Err(error(
                Code::Internal,
                "host returned an invalid file-read response",
            ));
        }
        let end = position
            .checked_add(data.len() as u64)
            .ok_or_else(|| error(Code::Internal, "file offset overflow"))?;
        if end > metadata.size || (reply.eof && end != metadata.size) {
            return Err(error(
                Code::RevisionConflict,
                "file size changed or host returned an inconsistent EOF",
            ));
        }
        for (index, byte) in data.iter().enumerate() {
            if index % 4096 == 0 {
                context.checkpoint()?;
            }
            if reader.feed(*byte)? {
                return Ok(result(path, revision, reader.output));
            }
        }
        position = end;
        if reply.eof {
            return Ok(result(path, revision, reader.finish(metadata.size == 0)?));
        }
    }
}

fn result(
    path: ExecutionPath,
    revision: execution_core::FileRevision,
    window: crate::TextReadOutput,
) -> ReadOutput {
    ReadOutput {
        path,
        revision,
        content: window.content,
        start_line: window.start_line,
        end_line: window.end_line,
        eof: window.eof,
        truncation: window.truncation,
        next_offset: window.next_offset,
    }
}
