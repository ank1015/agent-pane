use crate::error;
use execution_core::{
    ExecutionErrorCode as Code, ExecutionPath, ExecutionResult, ExecutionRuntime, FileKind,
    OperationContext, ReadFileRequest, StatRequest,
};

pub(crate) async fn read_image(
    runtime: &dyn ExecutionRuntime,
    context: &OperationContext,
    path: ExecutionPath,
    max_image_bytes: u64,
    chunk_bytes: u64,
) -> ExecutionResult<Vec<u8>> {
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
            "host returned metadata for a different image path",
        ));
    }
    if metadata.kind != FileKind::File {
        return Err(error(
            if metadata.kind == FileKind::Directory {
                Code::IsDirectory
            } else {
                Code::Unsupported
            },
            "image path is not a regular file",
        ));
    }
    if metadata.size > max_image_bytes {
        return Err(error(
            Code::ResourceExhausted,
            format!(
                "image contains {} bytes; view_image limit is {max_image_bytes}",
                metadata.size
            ),
        ));
    }
    usize::try_from(metadata.size).map_err(|_| {
        error(
            Code::ResourceExhausted,
            "image is too large for this tool process",
        )
    })?;
    let revision = metadata.revision.clone().ok_or_else(|| {
        error(
            Code::Unsupported,
            "host did not provide a file revision for a consistent image read",
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
    let mut position = 0u64;
    while position < metadata.size {
        context.checkpoint()?;
        let max_bytes = request_bytes.min(metadata.size - position);
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
                "image changed while reading; retry view_image",
            ));
        }
        let data = reply.data.as_slice();
        if reply.offset != position
            || reply.metadata.path != path
            || reply.metadata.kind != FileKind::File
            || data.len() as u64 > max_bytes
            || data.is_empty()
        {
            return Err(error(
                Code::Internal,
                "host returned an invalid image-read response",
            ));
        }
        let end = position
            .checked_add(data.len() as u64)
            .ok_or_else(|| error(Code::Internal, "image file offset overflow"))?;
        if end > metadata.size
            || reply.eof != (end == metadata.size)
            || bytes.len().checked_add(data.len()).is_none()
        {
            return Err(error(
                Code::RevisionConflict,
                "image size changed or host returned an inconsistent EOF",
            ));
        }
        bytes.try_reserve(data.len()).map_err(|_| {
            error(
                Code::ResourceExhausted,
                "image is too large for this tool process",
            )
        })?;
        bytes.extend_from_slice(data);
        position = end;
    }
    Ok(bytes)
}
