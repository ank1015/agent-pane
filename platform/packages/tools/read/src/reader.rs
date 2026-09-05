use crate::{FOOTER_RESERVE, ReadConfig, ReadOutput, Truncation, error};
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
    let mut output = ReadOutput {
        path: path.clone(),
        revision,
        content: String::new(),
        start_line: start,
        end_line: None,
        eof: false,
        truncation: None,
        next_offset: None,
    };
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
    let mut line_number = 1u64;
    let mut prefix_bytes = 7usize;
    let mut line = Vec::new();
    let mut rendered_bytes = 0usize;
    let budget = config.max_output_bytes - FOOTER_RESERVE;
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
        if reply.metadata.revision.as_ref() != Some(&output.revision)
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
            if line_number >= start
                && output
                    .end_line
                    .is_some_and(|last| last - start + 1 >= limit)
            {
                output.truncation = Some(Truncation::LineLimit);
                output.next_offset = Some(line_number);
                return Ok(output);
            }
            if *byte == 0 {
                return Err(error(
                    Code::Unsupported,
                    "file contains binary data (NUL); read supports UTF-8 text only",
                ));
            }
            if line_number >= start {
                line.push(*byte);
                // Reserve a newline for display even for an unterminated line.
                if rendered_bytes + prefix_bytes + line.len() + usize::from(*byte != b'\n') > budget
                {
                    if output.end_line.is_none() {
                        return Err(error(
                            Code::ResourceExhausted,
                            format!(
                                "line {line_number} exceeds the output limit; use bash to inspect part of this line"
                            ),
                        ));
                    }
                    output.truncation = Some(Truncation::ByteLimit);
                    output.next_offset = Some(line_number);
                    return Ok(output);
                }
                if *byte == b'\n' {
                    append(&mut output, &line, line_number, &mut rendered_bytes)?;
                    line.clear();
                }
            }
            if *byte == b'\n' {
                line_number = line_number
                    .checked_add(1)
                    .ok_or_else(|| error(Code::ResourceExhausted, "line number overflow"))?;
                prefix_bytes = line_number.to_string().len().max(6) + 1;
            }
        }
        position = end;
        if reply.eof {
            if !line.is_empty() {
                append(&mut output, &line, line_number, &mut rendered_bytes)?;
            }
            if output.end_line.is_none() && !(metadata.size == 0 && start == 1) {
                return Err(error(
                    Code::InvalidRequest,
                    format!("offset {start} is beyond the end of the file"),
                ));
            }
            output.eof = true;
            return Ok(output);
        }
    }
}

fn append(
    output: &mut ReadOutput,
    bytes: &[u8],
    number: u64,
    rendered_bytes: &mut usize,
) -> ExecutionResult<()> {
    let text = std::str::from_utf8(bytes).map_err(|_| {
        error(
            Code::Unsupported,
            "requested content is not valid UTF-8 text",
        )
    })?;
    *rendered_bytes +=
        number.to_string().len().max(6) + 1 + bytes.len() + usize::from(!bytes.ends_with(b"\n"));
    output.content.push_str(text);
    output.end_line = Some(number);
    Ok(())
}
