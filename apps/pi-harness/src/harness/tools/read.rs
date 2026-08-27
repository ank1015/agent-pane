use base64::{Engine, engine::general_purpose::STANDARD};
use execution_contracts::{
    ArtifactId, OpenArtifactRequest, ReadMode, ReadRequest, ReadResult, TextPageRequest,
};
use futures_util::StreamExt;
use llm_contracts::{
    Base64ImageSource, ContentPart, ImageContent, ImageSource, TextContent, ToolArguments,
    ToolDefinition,
};
use serde::Deserialize;
use serde_json::json;

use super::{
    ToolExecutionContext, ToolExecutionError, ToolOutput, function_tool, parse_arguments,
    resolve_path,
    truncate::{DEFAULT_MAX_BYTES, DEFAULT_MAX_LINES, format_size},
};

const ARTIFACT_CHUNK_BYTES: u64 = 1_024 * 1_024;

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ReadArguments {
    path: String,
    offset: Option<u64>,
    limit: Option<u32>,
}

pub fn definition() -> ToolDefinition {
    function_tool(
        "read",
        "Read the contents of a file in the current workspace. Supports text files and images (jpg, png, gif, webp, bmp). Images are sent as attachments. For text files, output is truncated to 2000 lines or 50KB (whichever is hit first). Use offset/limit for large files. When you need the full file, continue with offset until complete.",
        json!({
            "type": "object",
            "properties": {
                "path": {
                    "type": "string",
                    "description": "Path to the file to read (relative or absolute within the current workspace)"
                },
                "offset": {
                    "type": "number",
                    "description": "Line number to start reading from (1-indexed)"
                },
                "limit": {
                    "type": "number",
                    "description": "Maximum number of lines to read"
                }
            },
            "required": ["path"]
        }),
    )
}

pub async fn execute_read_tool(
    arguments: &ToolArguments,
    context: &ToolExecutionContext<'_>,
) -> Result<ToolOutput, ToolExecutionError> {
    let arguments: ReadArguments = parse_arguments("read", arguments)?;
    if arguments.offset == Some(0) {
        return Err(ToolExecutionError::invalid_arguments(
            "read",
            "offset must be one-based",
        ));
    }
    if arguments.limit == Some(0) {
        return Err(ToolExecutionError::invalid_arguments(
            "read",
            "limit must be greater than zero",
        ));
    }
    let path = resolve_path(context, &arguments.path)?;
    let start_line = arguments.offset.unwrap_or(1);
    let max_lines = arguments
        .limit
        .unwrap_or(DEFAULT_MAX_LINES as u32)
        .min(DEFAULT_MAX_LINES as u32);
    let result = context
        .runtime
        .workspace_query()
        .read(
            context.operation,
            ReadRequest {
                path,
                mode: ReadMode::Auto,
                page: Some(TextPageRequest {
                    start_line: Some(start_line),
                    cursor: None,
                    max_lines,
                    max_bytes: DEFAULT_MAX_BYTES as u64,
                    max_line_bytes: DEFAULT_MAX_BYTES as u64,
                    include_total_lines: true,
                }),
            },
        )
        .await?;

    match result {
        ReadResult::Text { page } => {
            let total_lines = page.total_lines.unwrap_or(page.end_line);
            if arguments.offset.is_some() && start_line > total_lines.max(1) {
                return Err(ToolExecutionError::invalid_arguments(
                    "read",
                    format!(
                        "offset {start_line} is beyond end of file ({total_lines} lines total)"
                    ),
                ));
            }

            let mut output = page.content;
            if page.lines_truncated {
                output.push_str(&format!(
                    "\n\n[One or more lines exceeded the {} per-line limit.]",
                    format_size(DEFAULT_MAX_BYTES)
                ));
            }
            if page.has_more {
                let next_line = page.next_line.unwrap_or(page.end_line.saturating_add(1));
                let user_limit_stopped_early = arguments
                    .limit
                    .is_some_and(|limit| limit <= DEFAULT_MAX_LINES as u32);
                if user_limit_stopped_early {
                    let remaining = total_lines.saturating_sub(page.end_line);
                    output.push_str(&format!(
                        "\n\n[{remaining} more lines in file. Use offset={next_line} to continue.]"
                    ));
                } else {
                    output.push_str(&format!(
                        "\n\n[Showing lines {}-{} of {total_lines} ({} limit). Use offset={next_line} to continue.]",
                        page.start_line,
                        page.end_line,
                        format_size(DEFAULT_MAX_BYTES)
                    ));
                }
            }

            let truncated = page.has_more || page.lines_truncated;
            let details = truncated.then(|| {
                json!({
                    "truncated": true,
                    "start_line": page.start_line,
                    "end_line": page.end_line,
                    "total_lines": total_lines,
                    "next_line": page.next_line,
                    "lines_truncated": page.lines_truncated,
                    "max_lines": max_lines,
                    "max_bytes": DEFAULT_MAX_BYTES
                })
            });
            let output = ToolOutput::text(output);
            Ok(match details {
                Some(details) => output.with_details(details),
                None => output,
            })
        }
        ReadResult::Media { artifact } if is_supported_image(&artifact.mime_type) => {
            let bytes = read_artifact(context, artifact.artifact_id.clone()).await?;
            Ok(ToolOutput {
                content: vec![
                    ContentPart::Text(TextContent {
                        content: format!("Read image file [{}]", artifact.mime_type),
                        metadata: None,
                    }),
                    ContentPart::Image(ImageContent {
                        source: ImageSource::Base64(Base64ImageSource {
                            data: STANDARD.encode(bytes),
                            mime_type: artifact.mime_type,
                        }),
                        detail: None,
                        metadata: None,
                    }),
                ],
                details: Some(json!({
                    "artifact_id": artifact.artifact_id,
                    "size": artifact.metadata.size
                })),
            })
        }
        ReadResult::Media { artifact } => Err(ToolExecutionError::process(format!(
            "read does not support media type `{}`",
            artifact.mime_type
        ))),
        ReadResult::Binary { artifact } => Err(ToolExecutionError::process(format!(
            "cannot read binary file `{}` as text",
            arguments.path
        ))
        .with_details(json!({
            "artifact_id": artifact.artifact_id,
            "mime_type": artifact.mime_type,
            "size": artifact.metadata.size
        }))),
    }
}

async fn read_artifact(
    context: &ToolExecutionContext<'_>,
    artifact_id: ArtifactId,
) -> Result<Vec<u8>, ToolExecutionError> {
    let store = context
        .runtime
        .artifact_store()
        .ok_or_else(|| ToolExecutionError::missing_capability("artifact reads"))?;
    let mut chunks = store
        .open(
            context.operation,
            OpenArtifactRequest {
                artifact_id,
                offset: 0,
                max_bytes: ARTIFACT_CHUNK_BYTES,
            },
        )
        .await?;
    let mut bytes = Vec::new();
    let mut expected_offset = 0_u64;
    loop {
        let chunk = tokio::select! {
            _ = context.operation.cancelled() => {
                return Err(ToolExecutionError::cancelled("image read was cancelled"));
            }
            chunk = chunks.next() => chunk,
        };
        let Some(chunk) = chunk else {
            break;
        };
        let chunk = chunk?;
        if chunk.offset != expected_offset {
            return Err(ToolExecutionError::process(format!(
                "artifact stream was out of order: expected offset {expected_offset}, received {}",
                chunk.offset
            )));
        }
        let chunk_bytes = STANDARD.decode(chunk.data.0).map_err(|error| {
            ToolExecutionError::process(format!("artifact contained invalid base64 data: {error}"))
        })?;
        expected_offset = expected_offset.saturating_add(chunk_bytes.len() as u64);
        bytes.extend_from_slice(&chunk_bytes);
        if chunk.eof {
            break;
        }
    }
    Ok(bytes)
}

fn is_supported_image(mime_type: &str) -> bool {
    matches!(
        mime_type,
        "image/jpeg" | "image/png" | "image/gif" | "image/webp" | "image/bmp"
    )
}
