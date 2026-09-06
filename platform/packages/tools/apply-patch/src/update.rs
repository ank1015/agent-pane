use crate::{ApplyPatchFileUpdateMode, parser::UpdateFileChunk};
use execution_core::{ExecutionErrorCode as Code, ExecutionResult};

type Replacement = (usize, usize, Vec<String>);

pub(crate) fn derive_new_contents(
    path: &str,
    original: &str,
    chunks: &[UpdateFileChunk],
    mode: ApplyPatchFileUpdateMode,
) -> ExecutionResult<String> {
    match mode {
        ApplyPatchFileUpdateMode::NormalizeToLf => {
            let mut original_lines = original.split('\n').map(String::from).collect::<Vec<_>>();
            if original_lines.last().is_some_and(String::is_empty) {
                original_lines.pop();
            }
            let replacements = compute_replacements(&original_lines, path, chunks, mode)?;
            let mut new_lines = apply_replacements(original_lines, &replacements);
            if !new_lines.last().is_some_and(String::is_empty) {
                new_lines.push(String::new());
            }
            Ok(new_lines.join("\n"))
        }
        ApplyPatchFileUpdateMode::PreserveLineEndings => {
            let mut source = SourceFile::parse(original);
            let replacements = compute_replacements(&source.line_texts(), path, chunks, mode)?;
            source.apply_replacements(&replacements);
            Ok(source.into_contents())
        }
    }
}

fn compute_replacements(
    original_lines: &[String],
    path: &str,
    chunks: &[UpdateFileChunk],
    mode: ApplyPatchFileUpdateMode,
) -> ExecutionResult<Vec<Replacement>> {
    let mut replacements = Vec::new();
    let mut line_index = 0usize;

    for chunk in chunks {
        if let Some(context) = &chunk.change_context {
            if let Some(index) = seek_sequence(
                original_lines,
                std::slice::from_ref(context),
                line_index,
                false,
                mode,
            ) {
                line_index = index + 1;
            } else {
                return Err(crate::error(
                    Code::InvalidRequest,
                    format!("Failed to find context '{context}' in {path}"),
                ));
            }
        }

        if chunk.old_lines.is_empty() {
            let insertion = match mode {
                ApplyPatchFileUpdateMode::NormalizeToLf
                    if original_lines.last().is_some_and(String::is_empty) =>
                {
                    original_lines.len() - 1
                }
                ApplyPatchFileUpdateMode::NormalizeToLf
                | ApplyPatchFileUpdateMode::PreserveLineEndings => original_lines.len(),
            };
            replacements.push((insertion, 0, chunk.new_lines.clone()));
            continue;
        }

        let mut pattern = chunk.old_lines.as_slice();
        let mut replacement = chunk.new_lines.as_slice();
        let mut found = seek_sequence(
            original_lines,
            pattern,
            line_index,
            chunk.is_end_of_file,
            mode,
        );
        if found.is_none() && pattern.last().is_some_and(String::is_empty) {
            pattern = &pattern[..pattern.len() - 1];
            if replacement.last().is_some_and(String::is_empty) {
                replacement = &replacement[..replacement.len() - 1];
            }
            found = seek_sequence(
                original_lines,
                pattern,
                line_index,
                chunk.is_end_of_file,
                mode,
            );
        }

        let Some(start) = found else {
            return Err(crate::error(
                Code::InvalidRequest,
                format!(
                    "Failed to find expected lines in {path}:\n{}",
                    chunk.old_lines.join("\n")
                ),
            ));
        };
        match mode {
            ApplyPatchFileUpdateMode::NormalizeToLf => {
                replacements.push((start, pattern.len(), replacement.to_vec()));
            }
            ApplyPatchFileUpdateMode::PreserveLineEndings => {
                let mut old_start = 0;
                let mut new_start = 0;
                for &(old_context, new_context) in &chunk.context_line_indices {
                    if old_context >= pattern.len() || new_context >= replacement.len() {
                        break;
                    }
                    if old_start != old_context || new_start != new_context {
                        replacements.push((
                            start + old_start,
                            old_context - old_start,
                            replacement[new_start..new_context].to_vec(),
                        ));
                    }
                    old_start = old_context + 1;
                    new_start = new_context + 1;
                }
                if old_start != pattern.len() || new_start != replacement.len() {
                    replacements.push((
                        start + old_start,
                        pattern.len() - old_start,
                        replacement[new_start..].to_vec(),
                    ));
                }
            }
        }
        line_index = start + pattern.len();
    }
    replacements.sort_by_key(|(index, _, _)| *index);
    Ok(replacements)
}

fn apply_replacements(mut lines: Vec<String>, replacements: &[Replacement]) -> Vec<String> {
    for (start, old_len, new_lines) in replacements.iter().rev() {
        // Legacy EOF matching can overlap a prior chunk. Match Codex's
        // bounds-checked removals rather than splicing a now-invalid range.
        for _ in 0..*old_len {
            if *start < lines.len() {
                lines.remove(*start);
            }
        }
        for (offset, line) in new_lines.iter().enumerate() {
            lines.insert(start + offset, line.clone());
        }
    }
    lines
}

fn seek_sequence(
    lines: &[String],
    pattern: &[String],
    start: usize,
    eof: bool,
    mode: ApplyPatchFileUpdateMode,
) -> Option<usize> {
    if pattern.is_empty() {
        return Some(start);
    }
    if pattern.len() > lines.len() {
        return None;
    }
    let end = lines.len() - pattern.len();
    let search_start = if eof {
        match mode {
            ApplyPatchFileUpdateMode::NormalizeToLf => end,
            ApplyPatchFileUpdateMode::PreserveLineEndings => end.max(start),
        }
    } else {
        start
    };
    if search_start > end {
        return None;
    }

    let matchers: [fn(&str) -> String; 3] = [
        str::to_owned,
        |value| value.trim_end().to_owned(),
        |value| value.trim().to_owned(),
    ];
    for normalize in matchers {
        for index in search_start..=end {
            if lines[index..index + pattern.len()]
                .iter()
                .zip(pattern)
                .all(|(actual, expected)| normalize(actual) == normalize(expected))
            {
                return Some(index);
            }
        }
    }
    for index in search_start..=end {
        if lines[index..index + pattern.len()]
            .iter()
            .zip(pattern)
            .all(|(actual, expected)| normalize_unicode(actual) == normalize_unicode(expected))
        {
            return Some(index);
        }
    }
    None
}

fn normalize_unicode(value: &str) -> String {
    value
        .trim()
        .chars()
        .map(|character| match character {
            '\u{2010}' | '\u{2011}' | '\u{2012}' | '\u{2013}' | '\u{2014}' | '\u{2015}'
            | '\u{2212}' => '-',
            '\u{2018}' | '\u{2019}' | '\u{201A}' | '\u{201B}' => '\'',
            '\u{201C}' | '\u{201D}' | '\u{201E}' | '\u{201F}' => '"',
            '\u{00A0}' | '\u{2002}' | '\u{2003}' | '\u{2004}' | '\u{2005}' | '\u{2006}'
            | '\u{2007}' | '\u{2008}' | '\u{2009}' | '\u{200A}' | '\u{202F}' | '\u{205F}'
            | '\u{3000}' => ' ',
            other => other,
        })
        .collect()
}

#[derive(Clone, Copy)]
enum LineEnding {
    Lf,
    CrLf,
    Cr,
}

impl LineEnding {
    const fn as_str(self) -> &'static str {
        match self {
            Self::Lf => "\n",
            Self::CrLf => "\r\n",
            Self::Cr => "\r",
        }
    }
}

struct SourceLine {
    text: String,
    ending: Option<LineEnding>,
}

struct SourceFile {
    lines: Vec<SourceLine>,
    preferred_ending: LineEnding,
}

impl SourceFile {
    fn parse(contents: &str) -> Self {
        let mut lines = Vec::new();
        let mut preferred = None;
        let mut line_start = 0;
        let mut cursor = 0;
        while cursor < contents.len() {
            let (ending, length) = match contents.as_bytes()[cursor] {
                b'\r' if contents.as_bytes().get(cursor + 1) == Some(&b'\n') => {
                    (LineEnding::CrLf, 2)
                }
                b'\r' => (LineEnding::Cr, 1),
                b'\n' => (LineEnding::Lf, 1),
                _ => {
                    cursor += 1;
                    continue;
                }
            };
            preferred.get_or_insert(ending);
            lines.push(SourceLine {
                text: contents[line_start..cursor].to_string(),
                ending: Some(ending),
            });
            cursor += length;
            line_start = cursor;
        }
        if line_start < contents.len() {
            lines.push(SourceLine {
                text: contents[line_start..].to_string(),
                ending: None,
            });
        }
        Self {
            lines,
            preferred_ending: preferred.unwrap_or(LineEnding::Lf),
        }
    }

    fn line_texts(&self) -> Vec<String> {
        self.lines.iter().map(|line| line.text.clone()).collect()
    }

    fn apply_replacements(&mut self, replacements: &[Replacement]) {
        let mut source_lines = std::mem::take(&mut self.lines).into_iter();
        let mut new_lines = Vec::new();
        let mut source_index = 0;
        for (start, old_len, new_segment) in replacements {
            new_lines.extend(source_lines.by_ref().take(*start - source_index));
            source_lines.by_ref().take(*old_len).for_each(drop);
            new_lines.extend(new_segment.iter().map(|text| SourceLine {
                text: text.clone(),
                ending: Some(self.preferred_ending),
            }));
            source_index = start + old_len;
        }
        new_lines.extend(source_lines);
        self.lines = new_lines;
        for line in &mut self.lines {
            line.ending.get_or_insert(self.preferred_ending);
        }
    }

    fn into_contents(self) -> String {
        let mut contents = String::new();
        for line in self.lines {
            contents.push_str(&line.text);
            if let Some(ending) = line.ending {
                contents.push_str(ending.as_str());
            }
        }
        contents
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn update_matches_codex_line_modes() {
        let chunks = [UpdateFileChunk {
            old_lines: vec!["one".into()],
            new_lines: vec!["uno".into()],
            ..Default::default()
        }];
        assert_eq!(
            derive_new_contents(
                "file",
                "one\r\ntwo\r\n",
                &chunks,
                ApplyPatchFileUpdateMode::NormalizeToLf
            )
            .expect("normalize"),
            "uno\ntwo\r\n"
        );
        assert_eq!(
            derive_new_contents(
                "file",
                "one\r\ntwo\r\n",
                &chunks,
                ApplyPatchFileUpdateMode::PreserveLineEndings
            )
            .expect("preserve"),
            "uno\r\ntwo\r\n"
        );
    }
}
