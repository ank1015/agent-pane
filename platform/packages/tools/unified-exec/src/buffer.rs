use execution_core::BinaryData;
use serde::{Deserialize, Serialize};

/// Serializable bounded output retaining a stable prefix and newest suffix.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct HeadTailBuffer {
    head: BinaryData,
    tail: BinaryData,
    max_bytes: usize,
    omitted_bytes: u64,
}

impl HeadTailBuffer {
    pub(crate) fn new(max_bytes: usize) -> Self {
        Self {
            head: BinaryData::default(),
            tail: BinaryData::default(),
            max_bytes,
            omitted_bytes: 0,
        }
    }

    fn head_budget(&self) -> usize {
        self.max_bytes / 2
    }

    fn tail_budget(&self) -> usize {
        self.max_bytes.saturating_sub(self.head_budget())
    }

    pub(crate) fn retained_bytes(&self) -> usize {
        self.head.len().saturating_add(self.tail.len())
    }

    pub(crate) fn total_bytes(&self) -> u64 {
        u64::try_from(self.retained_bytes())
            .unwrap_or(u64::MAX)
            .saturating_add(self.omitted_bytes)
    }

    pub(crate) fn omitted_bytes(&self) -> u64 {
        self.omitted_bytes
    }

    pub(crate) fn push(&mut self, input: &[u8]) {
        let head_remaining = self.head_budget().saturating_sub(self.head.len());
        let head_len = input.len().min(head_remaining);
        if head_len > 0 {
            let mut head = self.head.as_slice().to_vec();
            head.extend_from_slice(&input[..head_len]);
            self.head = BinaryData::new(head);
        }

        let input = &input[head_len..];
        if input.is_empty() {
            return;
        }

        let tail_budget = self.tail_budget();
        let previous = self.tail.as_slice();
        let combined_len = previous.len().saturating_add(input.len());
        let omitted = combined_len.saturating_sub(tail_budget);
        self.omitted_bytes = self
            .omitted_bytes
            .saturating_add(u64::try_from(omitted).unwrap_or(u64::MAX));

        let keep_previous = previous.len().saturating_sub(omitted.min(previous.len()));
        let skipped_input = omitted.saturating_sub(previous.len());
        let mut tail = Vec::with_capacity(tail_budget.min(combined_len));
        tail.extend_from_slice(&previous[previous.len().saturating_sub(keep_previous)..]);
        tail.extend_from_slice(&input[skipped_input.min(input.len())..]);
        self.tail = BinaryData::new(tail);
    }

    pub(crate) fn text(&self) -> String {
        let mut bytes = Vec::with_capacity(
            self.retained_bytes()
                .saturating_add(if self.omitted_bytes > 0 { 48 } else { 0 }),
        );
        bytes.extend_from_slice(self.head.as_slice());
        if self.omitted_bytes > 0 {
            bytes.extend_from_slice(
                format!("\n... {} bytes omitted ...\n", self.omitted_bytes).as_bytes(),
            );
        }
        bytes.extend_from_slice(self.tail.as_slice());
        String::from_utf8_lossy(&bytes).into_owned()
    }
}

#[cfg(test)]
mod tests {
    use super::HeadTailBuffer;

    #[test]
    fn retains_equal_head_and_tail_and_counts_omissions() {
        let mut buffer = HeadTailBuffer::new(10);
        buffer.push(b"0123");
        buffer.push(b"456789abcdef");
        assert_eq!(buffer.retained_bytes(), 10);
        assert_eq!(buffer.total_bytes(), 16);
        assert_eq!(buffer.omitted_bytes(), 6);
        assert_eq!(buffer.text(), "01234\n... 6 bytes omitted ...\nbcdef");
    }

    #[test]
    fn tolerates_zero_capacity_without_losing_the_total() {
        let mut buffer = HeadTailBuffer::new(0);
        buffer.push(b"hello");
        assert_eq!(buffer.total_bytes(), 5);
        assert_eq!(buffer.omitted_bytes(), 5);
    }
}
