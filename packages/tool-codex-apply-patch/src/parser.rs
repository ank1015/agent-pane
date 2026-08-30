//! Parser for Codex's model-facing apply-patch dialect.

use std::path::PathBuf;

use thiserror::Error;

const BEGIN_PATCH_MARKER: &str = "*** Begin Patch";
const END_PATCH_MARKER: &str = "*** End Patch";
const ADD_FILE_MARKER: &str = "*** Add File: ";
const DELETE_FILE_MARKER: &str = "*** Delete File: ";
const UPDATE_FILE_MARKER: &str = "*** Update File: ";
const MOVE_TO_MARKER: &str = "*** Move to: ";
const EOF_MARKER: &str = "*** End of File";
const CHANGE_CONTEXT_MARKER: &str = "@@ ";
const EMPTY_CHANGE_CONTEXT_MARKER: &str = "@@";

#[derive(Clone, Debug, Eq, Error, PartialEq)]
pub enum ParseError {
    #[error("invalid patch: {0}")]
    InvalidPatch(String),
    #[error("invalid hunk at line {line_number}, {message}")]
    InvalidHunk { message: String, line_number: usize },
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum Hunk {
    AddFile {
        path: PathBuf,
        contents: String,
    },
    DeleteFile {
        path: PathBuf,
    },
    UpdateFile {
        path: PathBuf,
        move_path: Option<PathBuf>,
        chunks: Vec<UpdateFileChunk>,
    },
}

impl Hunk {
    pub fn source_path(&self) -> &PathBuf {
        match self {
            Self::AddFile { path, .. }
            | Self::DeleteFile { path }
            | Self::UpdateFile { path, .. } => path,
        }
    }

    pub fn affected_path(&self) -> &PathBuf {
        match self {
            Self::UpdateFile {
                move_path: Some(path),
                ..
            } => path,
            _ => self.source_path(),
        }
    }
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
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

/// Parses one complete custom-tool input. Codex accepts a legacy heredoc
/// wrapper in addition to the grammar-constrained form, so this parser does too.
pub fn parse_patch(input: &str) -> Result<Vec<Hunk>, ParseError> {
    let lines: Vec<&str> = input.trim().lines().collect();
    let lines = check_boundaries_lenient(&lines)?;
    let patch = lines.join("\n");
    Parser::default().parse(&patch)
}

fn check_boundaries_lenient<'a>(lines: &'a [&'a str]) -> Result<&'a [&'a str], ParseError> {
    let original_error = match check_boundaries(lines) {
        Ok(lines) => return Ok(lines),
        Err(error) => error,
    };
    match lines {
        [first, .., last]
            if matches!(*first, "<<EOF" | "<<'EOF'" | "<<\"EOF\"")
                && last.ends_with("EOF")
                && lines.len() >= 4 =>
        {
            check_boundaries(&lines[1..lines.len() - 1])
        }
        _ => Err(original_error),
    }
}

fn check_boundaries<'a>(lines: &'a [&'a str]) -> Result<&'a [&'a str], ParseError> {
    let first = lines.first().map(|line| line.trim());
    let last = lines.last().map(|line| line.trim());
    match (first, last) {
        (Some(BEGIN_PATCH_MARKER), Some(END_PATCH_MARKER)) => Ok(lines),
        (Some(first), _) if first != BEGIN_PATCH_MARKER => Err(ParseError::InvalidPatch(
            "The first line of the patch must be '*** Begin Patch'".to_owned(),
        )),
        _ => Err(ParseError::InvalidPatch(
            "The last line of the patch must be '*** End Patch'".to_owned(),
        )),
    }
}

#[derive(Clone, Copy, Debug, Default)]
enum Mode {
    #[default]
    NotStarted,
    Started,
    AddFile,
    DeleteFile,
    UpdateFile {
        hunk_line_number: usize,
    },
    Ended,
}

#[derive(Default)]
struct Parser {
    mode: Mode,
    hunks: Vec<Hunk>,
    line_number: usize,
}

impl Parser {
    fn parse(mut self, patch: &str) -> Result<Vec<Hunk>, ParseError> {
        for line in patch.lines() {
            self.line_number += 1;
            self.process_line(line.trim_end_matches('\r'))?;
        }
        if !matches!(self.mode, Mode::Ended) {
            return Err(ParseError::InvalidPatch(
                "The last line of the patch must be '*** End Patch'".to_owned(),
            ));
        }
        Ok(self.hunks)
    }

    fn ensure_update_not_empty(&self, next_line: &str) -> Result<(), ParseError> {
        let Some(Hunk::UpdateFile { path, chunks, .. }) = self.hunks.last() else {
            return Ok(());
        };
        let Mode::UpdateFile { hunk_line_number } = self.mode else {
            return Ok(());
        };
        if chunks.is_empty() {
            return Err(ParseError::InvalidHunk {
                message: format!("Update file hunk for path '{}' is empty", path.display()),
                line_number: hunk_line_number,
            });
        }
        if chunks
            .last()
            .is_some_and(|chunk| chunk.old_lines.is_empty() && chunk.new_lines.is_empty())
        {
            let message = if next_line == END_PATCH_MARKER {
                "Update hunk does not contain any lines".to_owned()
            } else {
                format!(
                    "Unexpected line found in update hunk: '{next_line}'. Every line should start with ' ' (context line), '+' (added line), or '-' (removed line)"
                )
            };
            return Err(ParseError::InvalidHunk {
                message,
                line_number: self.line_number,
            });
        }
        Ok(())
    }

    fn handle_header(&mut self, line: &str) -> Result<bool, ParseError> {
        if line == END_PATCH_MARKER {
            self.ensure_update_not_empty(line)?;
            self.mode = Mode::Ended;
            return Ok(true);
        }
        if let Some(path) = line.strip_prefix(ADD_FILE_MARKER) {
            self.ensure_update_not_empty(line)?;
            self.hunks.push(Hunk::AddFile {
                path: PathBuf::from(path),
                contents: String::new(),
            });
            self.mode = Mode::AddFile;
            return Ok(true);
        }
        if let Some(path) = line.strip_prefix(DELETE_FILE_MARKER) {
            self.ensure_update_not_empty(line)?;
            self.hunks.push(Hunk::DeleteFile {
                path: PathBuf::from(path),
            });
            self.mode = Mode::DeleteFile;
            return Ok(true);
        }
        if let Some(path) = line.strip_prefix(UPDATE_FILE_MARKER) {
            self.ensure_update_not_empty(line)?;
            self.hunks.push(Hunk::UpdateFile {
                path: PathBuf::from(path),
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

    fn invalid_header(&self, line: &str) -> ParseError {
        ParseError::InvalidHunk {
            message: format!(
                "'{line}' is not a valid hunk header. Valid hunk headers: '*** Add File: {{path}}', '*** Delete File: {{path}}', '*** Update File: {{path}}'"
            ),
            line_number: self.line_number,
        }
    }

    fn process_line(&mut self, line: &str) -> Result<(), ParseError> {
        match self.mode {
            Mode::NotStarted => {
                if line.trim() == BEGIN_PATCH_MARKER {
                    self.mode = Mode::Started;
                    Ok(())
                } else {
                    Err(ParseError::InvalidPatch(
                        "The first line of the patch must be '*** Begin Patch'".to_owned(),
                    ))
                }
            }
            Mode::Started | Mode::AddFile | Mode::DeleteFile => {
                let trimmed = line.trim();
                if self.handle_header(trimmed)? {
                    return Ok(());
                }
                if matches!(self.mode, Mode::AddFile)
                    && let Some(added) = line.strip_prefix('+')
                    && let Some(Hunk::AddFile { contents, .. }) = self.hunks.last_mut()
                {
                    contents.push_str(added);
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
                    unreachable!("update mode always has an update hunk")
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
                    *move_path = Some(PathBuf::from(destination));
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
                        message: format!(
                            "Unexpected line found in update hunk: '{line}'. Every line should start with ' ' (context line), '+' (added line), or '-' (removed line)"
                        ),
                        line_number: self.line_number,
                    });
                }

                if update_line == EMPTY_CHANGE_CONTEXT_MARKER {
                    chunks.push(UpdateFileChunk::default());
                    return Ok(());
                }
                if let Some(context) = update_line.strip_prefix(CHANGE_CONTEXT_MARKER) {
                    chunks.push(UpdateFileChunk {
                        change_context: Some(context.to_owned()),
                        ..UpdateFileChunk::default()
                    });
                    return Ok(());
                }
                if update_line == EOF_MARKER {
                    if chunks.last().is_some_and(|chunk| {
                        chunk.old_lines.is_empty() && chunk.new_lines.is_empty()
                    }) {
                        return Err(ParseError::InvalidHunk {
                            message: "Update hunk does not contain any lines".to_owned(),
                            line_number: self.line_number,
                        });
                    }
                    if let Some(chunk) = chunks.last_mut() {
                        chunk.is_end_of_file = true;
                    }
                    return Ok(());
                }

                if line.is_empty() {
                    if chunks.is_empty() {
                        chunks.push(UpdateFileChunk::default());
                    }
                    chunks
                        .last_mut()
                        .expect("chunk was just inserted")
                        .push_context_line(String::new());
                    return Ok(());
                }
                if let Some(context) = line.strip_prefix(' ') {
                    if chunks.is_empty() {
                        chunks.push(UpdateFileChunk::default());
                    }
                    chunks
                        .last_mut()
                        .expect("chunk was just inserted")
                        .push_context_line(context.to_owned());
                    return Ok(());
                }
                if let Some(added) = line.strip_prefix('+') {
                    if chunks.is_empty() {
                        chunks.push(UpdateFileChunk::default());
                    }
                    chunks
                        .last_mut()
                        .expect("chunk was just inserted")
                        .new_lines
                        .push(added.to_owned());
                    return Ok(());
                }
                if let Some(removed) = line.strip_prefix('-') {
                    if chunks.is_empty() {
                        chunks.push(UpdateFileChunk::default());
                    }
                    chunks
                        .last_mut()
                        .expect("chunk was just inserted")
                        .old_lines
                        .push(removed.to_owned());
                    return Ok(());
                }
                let message = if chunks
                    .last()
                    .is_some_and(|chunk| !chunk.old_lines.is_empty() || !chunk.new_lines.is_empty())
                {
                    format!("Expected update hunk to start with a @@ context marker, got: '{line}'")
                } else {
                    format!(
                        "Unexpected line found in update hunk: '{line}'. Every line should start with ' ' (context line), '+' (added line), or '-' (removed line)"
                    )
                };
                Err(ParseError::InvalidHunk {
                    message,
                    line_number: self.line_number,
                })
            }
            Mode::Ended => {
                if line.trim().is_empty() {
                    Ok(())
                } else {
                    Err(ParseError::InvalidPatch(
                        "The last line of the patch must be '*** End Patch'".to_owned(),
                    ))
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_all_hunk_types() {
        let hunks = parse_patch(
            "*** Begin Patch\n*** Add File: add.txt\n+hello\n*** Delete File: old.txt\n*** Update File: from.txt\n*** Move to: to.txt\n@@ fn main\n-old\n+new\n*** End Patch",
        )
        .expect("valid patch");
        assert_eq!(hunks.len(), 3);
        assert!(matches!(hunks[0], Hunk::AddFile { .. }));
        assert!(matches!(hunks[1], Hunk::DeleteFile { .. }));
        assert!(matches!(hunks[2], Hunk::UpdateFile { .. }));
    }

    #[test]
    fn rejects_empty_update_hunk() {
        let error = parse_patch("*** Begin Patch\n*** Update File: empty.txt\n*** End Patch")
            .expect_err("empty update must fail");
        assert!(error.to_string().contains("empty"));
    }
}
