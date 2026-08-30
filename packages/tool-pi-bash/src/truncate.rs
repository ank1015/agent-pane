use serde::Serialize;

pub const DEFAULT_MAX_LINES: usize = 2_000;
pub const DEFAULT_MAX_BYTES: usize = 50 * 1_024;

#[derive(Debug, Serialize)]
#[serde(rename_all = "snake_case")]
pub struct TruncationResult {
    #[serde(skip)]
    pub content: String,
    pub truncated: bool,
    pub truncated_by: Option<TruncatedBy>,
    pub total_lines: usize,
    pub total_bytes: usize,
    pub output_lines: usize,
    pub output_bytes: usize,
    pub last_line_partial: bool,
    pub first_line_exceeds_limit: bool,
    pub max_lines: usize,
    pub max_bytes: usize,
}

#[derive(Clone, Copy, Debug, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum TruncatedBy {
    Lines,
    Bytes,
}

pub fn truncate_tail(content: &str) -> TruncationResult {
    let lines = split_lines(content);
    let total_lines = lines.len();
    let total_bytes = content.len();
    if total_lines <= DEFAULT_MAX_LINES && total_bytes <= DEFAULT_MAX_BYTES {
        return TruncationResult {
            content: content.to_owned(),
            truncated: false,
            truncated_by: None,
            total_lines,
            total_bytes,
            output_lines: total_lines,
            output_bytes: total_bytes,
            last_line_partial: false,
            first_line_exceeds_limit: false,
            max_lines: DEFAULT_MAX_LINES,
            max_bytes: DEFAULT_MAX_BYTES,
        };
    }

    let mut output = Vec::new();
    let mut output_bytes = 0;
    let mut truncated_by = TruncatedBy::Lines;
    let mut last_line_partial = false;
    for line in lines.iter().rev().take(DEFAULT_MAX_LINES) {
        let separator = usize::from(!output.is_empty());
        if output_bytes + line.len() + separator > DEFAULT_MAX_BYTES {
            truncated_by = TruncatedBy::Bytes;
            if output.is_empty() {
                let partial = tail_at_char_boundary(line, DEFAULT_MAX_BYTES);
                output_bytes = partial.len();
                output.push(partial);
                last_line_partial = true;
            }
            break;
        }
        output_bytes += line.len() + separator;
        output.push(*line);
    }
    output.reverse();
    if output.len() == DEFAULT_MAX_LINES && output_bytes <= DEFAULT_MAX_BYTES {
        truncated_by = TruncatedBy::Lines;
    }
    let content = output.join("\n");

    TruncationResult {
        output_bytes: content.len(),
        output_lines: output.len(),
        content,
        truncated: true,
        truncated_by: Some(truncated_by),
        total_lines,
        total_bytes,
        last_line_partial,
        first_line_exceeds_limit: false,
        max_lines: DEFAULT_MAX_LINES,
        max_bytes: DEFAULT_MAX_BYTES,
    }
}

pub fn format_size(bytes: usize) -> String {
    if bytes < 1_024 {
        format!("{bytes}B")
    } else if bytes < 1_024 * 1_024 {
        format!("{:.1}KB", bytes as f64 / 1_024.0)
    } else {
        format!("{:.1}MB", bytes as f64 / (1_024.0 * 1_024.0))
    }
}

fn split_lines(content: &str) -> Vec<&str> {
    if content.is_empty() {
        return Vec::new();
    }
    let mut lines: Vec<_> = content.split('\n').collect();
    if content.ends_with('\n') {
        lines.pop();
    }
    lines
}

fn tail_at_char_boundary(value: &str, max_bytes: usize) -> &str {
    if value.len() <= max_bytes {
        return value;
    }
    let mut start = value.len() - max_bytes;
    while !value.is_char_boundary(start) {
        start += 1;
    }
    &value[start..]
}

#[cfg(test)]
mod tests {
    use super::{DEFAULT_MAX_BYTES, DEFAULT_MAX_LINES, TruncatedBy, truncate_tail};

    #[test]
    fn keeps_the_tail_by_line_count() {
        let content = (0..=DEFAULT_MAX_LINES)
            .map(|line| line.to_string())
            .collect::<Vec<_>>()
            .join("\n");
        let result = truncate_tail(&content);

        assert!(result.truncated);
        assert!(matches!(result.truncated_by, Some(TruncatedBy::Lines)));
        assert!(!result.content.starts_with("0\n"));
        assert!(result.content.ends_with(&DEFAULT_MAX_LINES.to_string()));
    }

    #[test]
    fn keeps_valid_utf8_when_the_last_line_exceeds_the_byte_limit() {
        let content = "é".repeat(DEFAULT_MAX_BYTES);
        let result = truncate_tail(&content);

        assert!(result.last_line_partial);
        assert!(result.content.len() <= DEFAULT_MAX_BYTES);
        assert!(matches!(result.truncated_by, Some(TruncatedBy::Bytes)));
    }
}
