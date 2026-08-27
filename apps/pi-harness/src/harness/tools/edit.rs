use execution_contracts::{
    ApplyMutationRequest, ContentSource, ContinuationCursor, FileRevision, MutationAtomicity,
    MutationOperation, MutationPlan, MutationPostActions, ReadMode, ReadRequest, ReadResult,
    TextPageRequest,
};
use llm_contracts::{ToolArguments, ToolDefinition};
use serde::Deserialize;
use serde_json::json;

use super::{
    ToolExecutionContext, ToolExecutionError, ToolOutput, function_tool, operation_id,
    parse_arguments, resolve_path,
};

const EDIT_PAGE_LINES: u32 = 100_000;
const EDIT_PAGE_BYTES: u64 = 8 * 1_024 * 1_024;

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct EditArguments {
    path: String,
    edits: Vec<EditInput>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
struct EditInput {
    old_text: String,
    new_text: String,
}

struct MatchedEdit {
    index: usize,
    start: usize,
    end: usize,
    new_text: String,
}

pub fn definition() -> ToolDefinition {
    function_tool(
        "edit",
        "Edit a single file using exact text replacement. Every edits[].oldText must match a unique, non-overlapping region of the original file. If two changes affect the same block or nearby lines, merge them into one edit instead of emitting overlapping edits. Do not include large unchanged regions just to connect distant changes.",
        json!({
            "type": "object",
            "properties": {
                "path": {
                    "type": "string",
                    "description": "Path to the file to edit (relative or absolute within the current workspace)"
                },
                "edits": {
                    "type": "array",
                    "description": "One or more targeted replacements. Each edit is matched against the original file, not incrementally. Do not include overlapping or nested edits. If two changes touch the same block or nearby lines, merge them into one edit instead.",
                    "items": {
                        "type": "object",
                        "properties": {
                            "oldText": {
                                "type": "string",
                                "description": "Exact text for one targeted replacement. It must be unique in the original file and must not overlap with any other edits[].oldText in the same call."
                            },
                            "newText": {
                                "type": "string",
                                "description": "Replacement text for this targeted edit."
                            }
                        },
                        "required": ["oldText", "newText"]
                    }
                }
            },
            "required": ["path", "edits"]
        }),
    )
}

pub async fn execute_edit_tool(
    arguments: &ToolArguments,
    context: &ToolExecutionContext<'_>,
) -> Result<ToolOutput, ToolExecutionError> {
    let arguments: EditArguments = parse_arguments("edit", arguments)?;
    if arguments.edits.is_empty() {
        return Err(ToolExecutionError::invalid_arguments(
            "edit",
            "edits must contain at least one replacement",
        ));
    }
    let path = resolve_path(context, &arguments.path)?;
    let (raw_content, revision) = read_complete_text(context, path.clone()).await?;
    let (bom, content) = strip_bom(&raw_content);
    let line_ending = detect_line_ending(content);
    let normalized_content = normalize_line_endings(content);
    let edits = normalize_edits(arguments.edits);
    let mut matches = match_edits(&normalized_content, edits, &arguments.path)?;
    let first_changed_line = matches
        .iter()
        .map(|matched| line_number(&normalized_content, matched.start))
        .min();
    matches.sort_by_key(|matched| matched.start);
    reject_overlaps(&matches, &arguments.path)?;

    let mut new_content = normalized_content.clone();
    for matched in matches.iter().rev() {
        new_content.replace_range(matched.start..matched.end, &matched.new_text);
    }
    if new_content == normalized_content {
        return Err(ToolExecutionError::tool(
            "edit_failed",
            format!(
                "No changes made to {}. The replacements produced identical content.",
                arguments.path
            ),
        ));
    }
    let new_content = restore_line_endings(&new_content, line_ending);
    let final_content = if bom {
        format!("\u{feff}{new_content}")
    } else {
        new_content
    };

    let mutation = context
        .runtime
        .workspace_mutation()
        .ok_or_else(|| ToolExecutionError::missing_capability("workspace mutations"))?;
    let result = mutation
        .apply(
            context.operation,
            ApplyMutationRequest {
                operation_id: operation_id("pi-edit"),
                plan: MutationPlan {
                    operations: vec![MutationOperation::PutFile {
                        path,
                        content: ContentSource::Text {
                            content: final_content,
                        },
                        create_parents: false,
                        preserve_utf8_bom: false,
                        expected_revision: revision,
                    }],
                    atomicity: MutationAtomicity::AtomicIfSupported,
                    post_actions: MutationPostActions::default(),
                },
            },
        )
        .await?;

    Ok(ToolOutput::text(format!(
        "Successfully replaced {} block(s) in {}.",
        matches.len(),
        arguments.path
    ))
    .with_details(json!({
        "first_changed_line": first_changed_line,
        "replacements": matches.len(),
        "mutation": result
    })))
}

async fn read_complete_text(
    context: &ToolExecutionContext<'_>,
    path: execution_contracts::PathSpec,
) -> Result<(String, Option<FileRevision>), ToolExecutionError> {
    let mut content = String::new();
    let mut revision = None;
    let mut start_line = Some(1);
    let mut cursor: Option<ContinuationCursor> = None;
    loop {
        let result = context
            .runtime
            .workspace_query()
            .read(
                context.operation,
                ReadRequest {
                    path: path.clone(),
                    mode: ReadMode::Text,
                    page: Some(TextPageRequest {
                        start_line,
                        cursor: cursor.clone(),
                        max_lines: EDIT_PAGE_LINES,
                        max_bytes: EDIT_PAGE_BYTES,
                        max_line_bytes: EDIT_PAGE_BYTES,
                        include_total_lines: false,
                    }),
                },
            )
            .await?;
        let ReadResult::Text { page } = result else {
            return Err(ToolExecutionError::tool(
                "edit_failed",
                "edit requires a UTF-8 text file",
            ));
        };
        if page.lines_truncated {
            return Err(ToolExecutionError::tool(
                "edit_failed",
                format!(
                    "file contains a line larger than the {} byte edit page limit",
                    EDIT_PAGE_BYTES
                ),
            ));
        }
        if let Some(expected) = &revision {
            if page.metadata.revision.as_ref() != Some(expected) {
                return Err(ToolExecutionError::tool(
                    "edit_failed",
                    "file changed while it was being read",
                ));
            }
        } else {
            revision = page.metadata.revision.clone();
        }
        content.push_str(&page.content);
        if !page.has_more {
            break;
        }
        if let Some(next_cursor) = page.cursor {
            cursor = Some(next_cursor);
            start_line = None;
        } else if let Some(next_line) = page.next_line {
            cursor = None;
            start_line = Some(next_line);
        } else {
            return Err(ToolExecutionError::process(
                "text read was truncated without a continuation",
            ));
        }
    }
    Ok((content, revision))
}

fn normalize_edits(edits: Vec<EditInput>) -> Vec<EditInput> {
    edits
        .into_iter()
        .map(|edit| EditInput {
            old_text: normalize_line_endings(&edit.old_text),
            new_text: normalize_line_endings(&edit.new_text),
        })
        .collect()
}

fn match_edits(
    content: &str,
    edits: Vec<EditInput>,
    path: &str,
) -> Result<Vec<MatchedEdit>, ToolExecutionError> {
    let total = edits.len();
    edits
        .into_iter()
        .enumerate()
        .map(|(index, edit)| {
            if edit.old_text.is_empty() {
                return Err(edit_error(
                    path,
                    index,
                    total,
                    "oldText must not be empty",
                ));
            }
            let positions: Vec<_> = content.match_indices(&edit.old_text).collect();
            if positions.is_empty() {
                return Err(edit_error(
                    path,
                    index,
                    total,
                    "the old text was not found; it must match exactly including whitespace and newlines",
                ));
            }
            if positions.len() > 1 {
                return Err(edit_error(
                    path,
                    index,
                    total,
                    &format!(
                        "found {} occurrences; oldText must be unique and needs more context",
                        positions.len()
                    ),
                ));
            }
            let start = positions[0].0;
            Ok(MatchedEdit {
                index,
                start,
                end: start + edit.old_text.len(),
                new_text: edit.new_text,
            })
        })
        .collect()
}

fn reject_overlaps(matches: &[MatchedEdit], path: &str) -> Result<(), ToolExecutionError> {
    for pair in matches.windows(2) {
        if pair[0].end > pair[1].start {
            return Err(ToolExecutionError::tool(
                "edit_failed",
                format!(
                    "edits[{}] and edits[{}] overlap in {path}. Merge them into one edit or target disjoint regions.",
                    pair[0].index, pair[1].index
                ),
            ));
        }
    }
    Ok(())
}

fn edit_error(path: &str, index: usize, total: usize, message: &str) -> ToolExecutionError {
    let target = if total == 1 {
        "replacement".to_owned()
    } else {
        format!("edits[{index}]")
    };
    ToolExecutionError::tool(
        "edit_failed",
        format!("Could not edit {path}: {target} is invalid because {message}."),
    )
}

fn strip_bom(content: &str) -> (bool, &str) {
    content
        .strip_prefix('\u{feff}')
        .map_or((false, content), |content| (true, content))
}

#[derive(Clone, Copy)]
enum LineEnding {
    Lf,
    Crlf,
}

fn detect_line_ending(content: &str) -> LineEnding {
    content.find('\n').map_or(LineEnding::Lf, |index| {
        if index > 0 && content.as_bytes()[index - 1] == b'\r' {
            LineEnding::Crlf
        } else {
            LineEnding::Lf
        }
    })
}

fn normalize_line_endings(content: &str) -> String {
    content.replace("\r\n", "\n").replace('\r', "\n")
}

fn restore_line_endings(content: &str, ending: LineEnding) -> String {
    match ending {
        LineEnding::Lf => content.to_owned(),
        LineEnding::Crlf => content.replace('\n', "\r\n"),
    }
}

fn line_number(content: &str, byte_offset: usize) -> usize {
    content.as_bytes()[..byte_offset]
        .iter()
        .filter(|value| **value == b'\n')
        .count()
        + 1
}
