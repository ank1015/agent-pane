use std::collections::VecDeque;

pub const OUTPUT_COLLECTION_MAX_BYTES: usize = 1024 * 1024;
pub const DEFAULT_MAX_OUTPUT_TOKENS: usize = 10_000;

#[derive(Debug, Default)]
pub(crate) struct HeadTailBuffer {
    head: Vec<u8>,
    tail: VecDeque<u8>,
    omitted_bytes: usize,
    total_bytes: usize,
}

impl HeadTailBuffer {
    const HEAD_BUDGET: usize = OUTPUT_COLLECTION_MAX_BYTES / 2;
    const TAIL_BUDGET: usize = OUTPUT_COLLECTION_MAX_BYTES - Self::HEAD_BUDGET;

    pub(crate) fn push(&mut self, bytes: &[u8]) {
        self.total_bytes = self.total_bytes.saturating_add(bytes.len());
        let remaining_head = Self::HEAD_BUDGET.saturating_sub(self.head.len());
        let split = remaining_head.min(bytes.len());
        self.head.extend_from_slice(&bytes[..split]);
        self.push_tail(&bytes[split..]);
    }

    fn push_tail(&mut self, bytes: &[u8]) {
        let remaining = Self::TAIL_BUDGET.saturating_sub(self.tail.len());
        let excess = bytes.len().saturating_sub(remaining);
        self.omitted_bytes = self.omitted_bytes.saturating_add(excess);
        if excess <= self.tail.len() {
            self.tail.drain(..excess);
            self.tail.extend(bytes);
        } else {
            let skip = excess - self.tail.len();
            self.tail.clear();
            self.tail.extend(&bytes[skip..]);
        }
    }

    pub(crate) const fn total_bytes(&self) -> usize {
        self.total_bytes
    }

    pub(crate) const fn omitted_bytes(&self) -> usize {
        self.omitted_bytes
    }

    pub(crate) fn into_bytes(self) -> Vec<u8> {
        let marker = omission_marker(self.omitted_bytes);
        let marker_bytes = usize::from(self.omitted_bytes > 0) * (marker.len() + 2);
        let mut bytes = Vec::with_capacity(self.head.len() + self.tail.len() + marker_bytes);
        bytes.extend(self.head);
        if self.omitted_bytes > 0 {
            bytes.push(b'\n');
            bytes.extend(marker.as_bytes());
            bytes.push(b'\n');
        }
        bytes.extend(self.tail);
        bytes
    }
}

pub(crate) fn approx_token_count_from_bytes(bytes: usize) -> usize {
    bytes.saturating_add(3) / 4
}

pub(crate) fn model_output(
    raw_output: &[u8],
    max_tokens: usize,
    original_token_count: usize,
    omitted_bytes: usize,
) -> String {
    let text = String::from_utf8_lossy(raw_output).into_owned();
    let budget = max_tokens.saturating_mul(4);
    if text.len() <= budget {
        return text;
    }

    let truncated = truncate_middle(&text, budget, true);
    let omission = if omitted_bytes > 0 && !truncated.contains(&omission_marker(omitted_bytes)) {
        format!("{}\n", omission_marker(omitted_bytes))
    } else {
        String::new()
    };
    format!(
        "Warning: truncated output (original token count: {original_token_count})\nTotal output lines: {}\n{omission}\n{truncated}",
        text.lines().count()
    )
}

pub(crate) fn omission_marker(bytes: usize) -> String {
    format!("... {bytes} bytes omitted ...")
}

fn truncate_middle(value: &str, max_bytes: usize, token_units: bool) -> String {
    if value.is_empty() || value.len() <= max_bytes {
        return value.to_owned();
    }
    let left_budget = max_bytes / 2;
    let right_budget = max_bytes - left_budget;

    let mut left_end = left_budget.min(value.len());
    while !value.is_char_boundary(left_end) {
        left_end = left_end.saturating_sub(1);
    }
    let target = value.len().saturating_sub(right_budget);
    let mut right_start = target.min(value.len());
    while right_start < value.len() && !value.is_char_boundary(right_start) {
        right_start += 1;
    }
    right_start = right_start.max(left_end);

    let removed = if token_units {
        approx_token_count_from_bytes(right_start.saturating_sub(left_end))
    } else {
        value[left_end..right_start].chars().count()
    };
    let unit = if token_units { "tokens" } else { "chars" };
    format!(
        "{}…{removed} {unit} truncated…{}",
        &value[..left_end],
        &value[right_start..]
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn head_tail_buffer_keeps_both_ends() {
        let mut buffer = HeadTailBuffer::default();
        buffer.push(&vec![b'a'; OUTPUT_COLLECTION_MAX_BYTES]);
        buffer.push(&[b'z'; 100]);
        let omitted = buffer.omitted_bytes();
        let output = String::from_utf8(buffer.into_bytes()).unwrap();
        assert!(omitted > 0);
        assert!(output.starts_with('a'));
        assert!(output.ends_with(&"z".repeat(100)));
        assert!(output.contains("bytes omitted"));
    }

    #[test]
    fn token_budget_keeps_prefix_and_suffix() {
        let value = format!("HEAD{}TAIL", "x".repeat(100));
        let output = model_output(value.as_bytes(), 5, 27, 0);
        assert!(output.contains("Warning: truncated output"));
        assert!(output.contains("HEAD"));
        assert!(output.ends_with("TAIL"));
    }
}
