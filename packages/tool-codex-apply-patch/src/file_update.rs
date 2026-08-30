use crate::parser::UpdateFileChunk;

type Replacement = (usize, usize, Vec<String>);

/// Derives Codex's default update result. Existing lines are split on LF, a
/// final empty split item is removed, and the reconstructed file ends in LF.
pub fn derive_new_contents(
    original_contents: &str,
    display_path: &str,
    chunks: &[UpdateFileChunk],
) -> Result<String, String> {
    let mut original_lines = original_contents
        .split('\n')
        .map(str::to_owned)
        .collect::<Vec<_>>();
    if original_lines.last().is_some_and(String::is_empty) {
        original_lines.pop();
    }

    let replacements = compute_replacements(&original_lines, display_path, chunks)?;
    let mut new_lines = apply_replacements(original_lines, &replacements);
    if !new_lines.last().is_some_and(String::is_empty) {
        new_lines.push(String::new());
    }
    Ok(new_lines.join("\n"))
}

fn compute_replacements(
    original_lines: &[String],
    path: &str,
    chunks: &[UpdateFileChunk],
) -> Result<Vec<Replacement>, String> {
    let mut replacements = Vec::new();
    let mut line_index = 0;

    for chunk in chunks {
        if let Some(context) = &chunk.change_context {
            if let Some(index) = seek_sequence(
                original_lines,
                std::slice::from_ref(context),
                line_index,
                false,
            ) {
                line_index = index + 1;
            } else {
                return Err(format!("Failed to find context '{context}' in {path}"));
            }
        }

        if chunk.old_lines.is_empty() {
            let insertion_index = if original_lines.last().is_some_and(String::is_empty) {
                original_lines.len() - 1
            } else {
                original_lines.len()
            };
            replacements.push((insertion_index, 0, chunk.new_lines.clone()));
            continue;
        }

        let mut pattern = chunk.old_lines.as_slice();
        let mut new_lines = chunk.new_lines.as_slice();
        let mut found = seek_sequence(original_lines, pattern, line_index, chunk.is_end_of_file);
        if found.is_none() && pattern.last().is_some_and(String::is_empty) {
            pattern = &pattern[..pattern.len() - 1];
            if new_lines.last().is_some_and(String::is_empty) {
                new_lines = &new_lines[..new_lines.len() - 1];
            }
            found = seek_sequence(original_lines, pattern, line_index, chunk.is_end_of_file);
        }

        let Some(start_index) = found else {
            return Err(format!(
                "Failed to find expected lines in {path}:\n{}",
                chunk.old_lines.join("\n")
            ));
        };
        replacements.push((start_index, pattern.len(), new_lines.to_vec()));
        line_index = start_index + pattern.len();
    }

    replacements.sort_by_key(|(index, _, _)| *index);
    Ok(replacements)
}

fn apply_replacements(mut lines: Vec<String>, replacements: &[Replacement]) -> Vec<String> {
    for (start_index, old_length, new_lines) in replacements.iter().rev() {
        for _ in 0..*old_length {
            if *start_index < lines.len() {
                lines.remove(*start_index);
            }
        }
        for (offset, line) in new_lines.iter().enumerate() {
            lines.insert(*start_index + offset, line.clone());
        }
    }
    lines
}

fn seek_sequence(lines: &[String], pattern: &[String], start: usize, eof: bool) -> Option<usize> {
    if pattern.is_empty() {
        return Some(start);
    }
    if pattern.len() > lines.len() {
        return None;
    }
    let search_start = if eof {
        lines.len() - pattern.len()
    } else {
        start
    };
    let final_start = lines.len().saturating_sub(pattern.len());
    if search_start > final_start {
        return None;
    }

    (search_start..=final_start)
        .find(|&index| lines[index..index + pattern.len()] == *pattern)
        .or_else(|| {
            (search_start..=final_start).find(|&index| {
                pattern.iter().enumerate().all(|(offset, expected)| {
                    lines[index + offset].trim_end() == expected.trim_end()
                })
            })
        })
        .or_else(|| {
            (search_start..=final_start).find(|&index| {
                pattern
                    .iter()
                    .enumerate()
                    .all(|(offset, expected)| lines[index + offset].trim() == expected.trim())
            })
        })
        .or_else(|| {
            (search_start..=final_start).find(|&index| {
                pattern.iter().enumerate().all(|(offset, expected)| {
                    normalize_unicode(&lines[index + offset]) == normalize_unicode(expected)
                })
            })
        })
}

fn normalize_unicode(value: &str) -> String {
    value
        .trim()
        .chars()
        .map(|character| match character {
            '\u{2010}' | '\u{2011}' | '\u{2012}' | '\u{2013}' | '\u{2014}' | '\u{2015}'
            | '\u{2212}' => '-',
            '\u{2018}' | '\u{2019}' | '\u{201a}' | '\u{201b}' => '\'',
            '\u{201c}' | '\u{201d}' | '\u{201e}' | '\u{201f}' => '"',
            '\u{00a0}' | '\u{2002}' | '\u{2003}' | '\u{2004}' | '\u{2005}' | '\u{2006}'
            | '\u{2007}' | '\u{2008}' | '\u{2009}' | '\u{200a}' | '\u{202f}' | '\u{205f}'
            | '\u{3000}' => ' ',
            other => other,
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn matches_context_with_codex_whitespace_fallbacks() {
        let chunks = [UpdateFileChunk {
            old_lines: vec!["beta".to_owned()],
            new_lines: vec!["BETA".to_owned()],
            ..UpdateFileChunk::default()
        }];
        assert_eq!(
            derive_new_contents("alpha\n  beta  \ngamma\n", "file.txt", &chunks).unwrap(),
            "alpha\nBETA\ngamma\n"
        );
    }

    #[test]
    fn end_of_file_prefers_the_last_match() {
        let chunks = [UpdateFileChunk {
            old_lines: vec!["same".to_owned()],
            new_lines: vec!["last".to_owned()],
            is_end_of_file: true,
            ..UpdateFileChunk::default()
        }];
        assert_eq!(
            derive_new_contents("same\nsame\n", "file.txt", &chunks).unwrap(),
            "same\nlast\n"
        );
    }
}
