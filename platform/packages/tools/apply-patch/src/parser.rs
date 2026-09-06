use std::fmt;

use serde::{Deserialize, Serialize};

const BEGIN_PATCH_MARKER: &str = "*** Begin Patch";
const END_PATCH_MARKER: &str = "*** End Patch";
const ADD_FILE_MARKER: &str = "*** Add File: ";
const DELETE_FILE_MARKER: &str = "*** Delete File: ";
const UPDATE_FILE_MARKER: &str = "*** Update File: ";
const MOVE_TO_MARKER: &str = "*** Move to: ";
const EOF_MARKER: &str = "*** End of File";
const CHANGE_CONTEXT_MARKER: &str = "@@ ";
const EMPTY_CHANGE_CONTEXT_MARKER: &str = "@@";
const ENVIRONMENT_ID_MARKER: &str = "*** Environment ID:";

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Hunk {
    AddFile {
        path: String,
        contents: String,
    },
    DeleteFile {
        path: String,
    },
    UpdateFile {
        path: String,
        move_path: Option<String>,
        chunks: Vec<UpdateFileChunk>,
    },
}

impl Hunk {
    #[must_use]
    pub fn source_path(&self) -> &str {
        match self {
            Self::AddFile { path, .. }
            | Self::DeleteFile { path }
            | Self::UpdateFile { path, .. } => path,
        }
    }

    #[must_use]
    pub fn affected_path(&self) -> &str {
        match self {
            Self::UpdateFile {
                move_path: Some(path),
                ..
            } => path,
            _ => self.source_path(),
        }
    }
}

#[derive(Clone, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct UpdateFileChunk {
    pub change_context: Option<String>,
    pub old_lines: Vec<String>,
    pub new_lines: Vec<String>,
    pub context_line_indices: Vec<(usize, usize)>,
    pub is_end_of_file: bool,
}

impl UpdateFileChunk {
    fn push_context_line(&mut self, line: String) {
        self.context_line_indices
            .push((self.old_lines.len(), self.new_lines.len()));
        self.old_lines.push(line.clone());
        self.new_lines.push(line);
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ParseError {
    InvalidPatch(String),
    InvalidHunk { message: String, line_number: usize },
}

impl fmt::Display for ParseError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidPatch(message) => write!(formatter, "invalid patch: {message}"),
            Self::InvalidHunk {
                message,
                line_number,
            } => write!(formatter, "invalid hunk at line {line_number}, {message}"),
        }
    }
}

impl std::error::Error for ParseError {}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ParsedPatch {
    pub patch: String,
    pub hunks: Vec<Hunk>,
    pub environment_id: Option<String>,
}

/// Parse Codex's freeform patch syntax. The parser intentionally accepts the
/// legacy `<<EOF` wrapper that Codex accepts for old shell-shaped calls.
pub fn parse_patch(input: &str) -> Result<ParsedPatch, ParseError> {
    let original_lines = input.trim().lines().collect::<Vec<_>>();
    let lines = patch_lines(&original_lines)?;
    let patch = lines.join("\n");
    let mut parser = Parser::default();
    for (index, line) in lines.iter().enumerate() {
        parser.line_number += 1;
        if index + 1 == lines.len() && line.trim() == END_PATCH_MARKER {
            parser.ensure_update_not_empty(END_PATCH_MARKER)?;
            parser.mode = Mode::EndedPatch;
        } else {
            parser.process_line(line.strip_suffix('\r').unwrap_or(line))?;
        }
    }
    if parser.mode != Mode::EndedPatch {
        return Err(ParseError::InvalidPatch(
            "The last line of the patch must be '*** End Patch'".to_string(),
        ));
    }
    Ok(ParsedPatch {
        patch,
        hunks: parser.hunks,
        environment_id: parser.environment_id,
    })
}

fn patch_lines<'a>(lines: &'a [&'a str]) -> Result<&'a [&'a str], ParseError> {
    if boundaries_valid(lines) {
        return Ok(lines);
    }
    if let [first, .., last] = lines
        && matches!(*first, "<<EOF" | "<<'EOF'" | "<<\"EOF\"")
        && last.ends_with("EOF")
        && lines.len() >= 4
    {
        let inner = &lines[1..lines.len() - 1];
        check_boundaries(inner)?;
        return Ok(inner);
    }
    check_boundaries(lines)?;
    unreachable!("boundary validation either returns or errors")
}

fn boundaries_valid(lines: &[&str]) -> bool {
    lines
        .first()
        .is_some_and(|line| line.trim() == BEGIN_PATCH_MARKER)
        && lines
            .last()
            .is_some_and(|line| line.trim() == END_PATCH_MARKER)
}

fn check_boundaries(lines: &[&str]) -> Result<(), ParseError> {
    if !lines
        .first()
        .is_some_and(|line| line.trim() == BEGIN_PATCH_MARKER)
    {
        return Err(ParseError::InvalidPatch(
            "The first line of the patch must be '*** Begin Patch'".to_string(),
        ));
    }
    if !lines
        .last()
        .is_some_and(|line| line.trim() == END_PATCH_MARKER)
    {
        return Err(ParseError::InvalidPatch(
            "The last line of the patch must be '*** End Patch'".to_string(),
        ));
    }
    Ok(())
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
enum Mode {
    #[default]
    NotStarted,
    StartedPatch,
    AddFile,
    DeleteFile,
    UpdateFile {
        hunk_line_number: usize,
    },
    EndedPatch,
}

#[derive(Default)]
struct Parser {
    mode: Mode,
    line_number: usize,
    hunks: Vec<Hunk>,
    environment_id: Option<String>,
}

impl Parser {
    fn ensure_update_not_empty(&self, line: &str) -> Result<(), ParseError> {
        if let Some(Hunk::UpdateFile { path, chunks, .. }) = self.hunks.last() {
            if chunks.is_empty()
                && let Mode::UpdateFile { hunk_line_number } = self.mode
            {
                return Err(ParseError::InvalidHunk {
                    message: format!("Update file hunk for path '{path}' is empty"),
                    line_number: hunk_line_number,
                });
            }
            if chunks
                .last()
                .is_some_and(|chunk| chunk.old_lines.is_empty() && chunk.new_lines.is_empty())
            {
                let message = if line == END_PATCH_MARKER {
                    "Update hunk does not contain any lines".to_string()
                } else {
                    invalid_update_line(line)
                };
                return Err(ParseError::InvalidHunk {
                    message,
                    line_number: self.line_number,
                });
            }
        }
        Ok(())
    }

    fn handle_header(&mut self, line: &str) -> Result<bool, ParseError> {
        if self.mode == Mode::StartedPatch
            && let Some(environment_id) = line.strip_prefix(ENVIRONMENT_ID_MARKER)
        {
            if self.environment_id.is_some() {
                return Err(ParseError::InvalidPatch(
                    "apply_patch environment_id cannot be specified more than once".to_string(),
                ));
            }
            let environment_id = environment_id.trim();
            if environment_id.is_empty() {
                return Err(ParseError::InvalidPatch(
                    "apply_patch environment_id cannot be empty".to_string(),
                ));
            }
            self.environment_id = Some(environment_id.to_string());
            return Ok(true);
        }
        if line == END_PATCH_MARKER {
            self.ensure_update_not_empty(line)?;
            self.mode = Mode::EndedPatch;
            return Ok(true);
        }
        if let Some(path) = line.strip_prefix(ADD_FILE_MARKER) {
            self.ensure_update_not_empty(line)?;
            self.hunks.push(Hunk::AddFile {
                path: path.to_string(),
                contents: String::new(),
            });
            self.mode = Mode::AddFile;
            return Ok(true);
        }
        if let Some(path) = line.strip_prefix(DELETE_FILE_MARKER) {
            self.ensure_update_not_empty(line)?;
            self.hunks.push(Hunk::DeleteFile {
                path: path.to_string(),
            });
            self.mode = Mode::DeleteFile;
            return Ok(true);
        }
        if let Some(path) = line.strip_prefix(UPDATE_FILE_MARKER) {
            self.ensure_update_not_empty(line)?;
            self.hunks.push(Hunk::UpdateFile {
                path: path.to_string(),
                move_path: None,
                chunks: Vec::new(),
            });
            self.mode = Mode::UpdateFile {
                hunk_line_number: self.line_number,
            };
            return Ok(true);
        }
        Ok(false)
    }

    fn process_line(&mut self, line: &str) -> Result<(), ParseError> {
        let trimmed = line.trim();
        match self.mode {
            Mode::NotStarted => {
                if trimmed == BEGIN_PATCH_MARKER {
                    self.mode = Mode::StartedPatch;
                    Ok(())
                } else {
                    Err(ParseError::InvalidPatch(
                        "The first line of the patch must be '*** Begin Patch'".to_string(),
                    ))
                }
            }
            Mode::StartedPatch | Mode::DeleteFile => {
                if self.handle_header(trimmed)? {
                    Ok(())
                } else {
                    Err(self.invalid_header(trimmed))
                }
            }
            Mode::AddFile => {
                if self.handle_header(trimmed)? {
                    return Ok(());
                }
                if let Some(content) = line.strip_prefix('+')
                    && let Some(Hunk::AddFile { contents, .. }) = self.hunks.last_mut()
                {
                    contents.push_str(content);
                    contents.push('\n');
                    return Ok(());
                }
                Err(self.invalid_header(trimmed))
            }
            Mode::UpdateFile { hunk_line_number } => {
                let update_line = line.trim_end();
                if self.handle_header(update_line)? {
                    return Ok(());
                }
                let Some(Hunk::UpdateFile {
                    move_path, chunks, ..
                }) = self.hunks.last_mut()
                else {
                    unreachable!("update mode has an update hunk")
                };

                if chunks.last().is_some_and(|chunk| chunk.is_end_of_file) {
                    if update_line.is_empty() {
                        return Ok(());
                    }
                    if update_line != EMPTY_CHANGE_CONTEXT_MARKER
                        && !update_line.starts_with(CHANGE_CONTEXT_MARKER)
                    {
                        return Err(ParseError::InvalidHunk {
                            message: format!(
                                "Expected update hunk to start with a @@ context marker, got: '{line}'"
                            ),
                            line_number: self.line_number,
                        });
                    }
                }
                if chunks.is_empty()
                    && move_path.is_none()
                    && let Some(destination) = update_line.strip_prefix(MOVE_TO_MARKER)
                {
                    *move_path = Some(destination.to_string());
                    self.mode = Mode::UpdateFile { hunk_line_number };
                    return Ok(());
                }
                if (update_line == EMPTY_CHANGE_CONTEXT_MARKER
                    || update_line.starts_with(CHANGE_CONTEXT_MARKER))
                    && chunks.last().is_some_and(|chunk| {
                        chunk.old_lines.is_empty() && chunk.new_lines.is_empty()
                    })
                {
                    return Err(ParseError::InvalidHunk {
                        message: invalid_update_line(line),
                        line_number: self.line_number,
                    });
                }
                if update_line == EMPTY_CHANGE_CONTEXT_MARKER {
                    chunks.push(UpdateFileChunk::default());
                    return Ok(());
                }
                if let Some(context) = update_line.strip_prefix(CHANGE_CONTEXT_MARKER) {
                    chunks.push(UpdateFileChunk {
                        change_context: Some(context.to_string()),
                        ..Default::default()
                    });
                    return Ok(());
                }
                if update_line == EOF_MARKER {
                    if chunks.last().is_some_and(|chunk| {
                        chunk.old_lines.is_empty() && chunk.new_lines.is_empty()
                    }) {
                        return Err(ParseError::InvalidHunk {
                            message: "Update hunk does not contain any lines".to_string(),
                            line_number: self.line_number,
                        });
                    }
                    if let Some(chunk) = chunks.last_mut() {
                        chunk.is_end_of_file = true;
                    }
                    return Ok(());
                }
                if line.is_empty() {
                    chunks
                        .get_or_insert_default()
                        .push_context_line(String::new());
                    return Ok(());
                }
                if let Some(context) = line.strip_prefix(' ') {
                    chunks
                        .get_or_insert_default()
                        .push_context_line(context.to_string());
                    return Ok(());
                }
                if let Some(added) = line.strip_prefix('+') {
                    chunks
                        .get_or_insert_default()
                        .new_lines
                        .push(added.to_string());
                    return Ok(());
                }
                if let Some(removed) = line.strip_prefix('-') {
                    chunks
                        .get_or_insert_default()
                        .old_lines
                        .push(removed.to_string());
                    return Ok(());
                }
                let message = if chunks
                    .last()
                    .is_some_and(|chunk| !chunk.old_lines.is_empty() || !chunk.new_lines.is_empty())
                {
                    format!("Expected update hunk to start with a @@ context marker, got: '{line}'")
                } else {
                    invalid_update_line(line)
                };
                Err(ParseError::InvalidHunk {
                    message,
                    line_number: self.line_number,
                })
            }
            Mode::EndedPatch => {
                if trimmed.is_empty() {
                    Ok(())
                } else {
                    Err(ParseError::InvalidPatch(
                        "The last line of the patch must be '*** End Patch'".to_string(),
                    ))
                }
            }
        }
    }

    fn invalid_header(&self, line: &str) -> ParseError {
        ParseError::InvalidHunk {
            message: format!(
                "'{line}' is not a valid hunk header. Valid hunk headers: '*** Add File: {{path}}', '*** Delete File: {{path}}', '*** Update File: {{path}}'"
            ),
            line_number: self.line_number,
        }
    }
}

trait LastChunk {
    fn get_or_insert_default(&mut self) -> &mut UpdateFileChunk;
}

impl LastChunk for Vec<UpdateFileChunk> {
    fn get_or_insert_default(&mut self) -> &mut UpdateFileChunk {
        if self.is_empty() {
            self.push(UpdateFileChunk::default());
        }
        self.last_mut().expect("a chunk was inserted")
    }
}

fn invalid_update_line(line: &str) -> String {
    format!(
        "Unexpected line found in update hunk: '{line}'. Every line should start with ' ' (context line), '+' (added line), or '-' (removed line)"
    )
}
