use execution_core::{BinaryData, ExecutionHandle, ExecutionHostId, ExecutionPath, ExecutionState};
use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct BashOutput {
    pub host_id: ExecutionHostId,
    pub handle: ExecutionHandle,
    pub cwd: ExecutionPath,
    pub output: String,
    pub truncated: bool,
    pub output_bytes: u64,
    pub exit_code: Option<i32>,
    pub failure: Option<String>,
    /// False for snapshots while running or after a transport failure.
    pub complete: bool,
}

impl BashOutput {
    pub fn is_error(&self) -> bool {
        self.complete && (self.handle.state != ExecutionState::Exited || self.exit_code != Some(0))
    }

    pub fn to_text(&self) -> String {
        let mut text = self.output.clone();
        if !text.is_empty() && !text.ends_with('\n') {
            text.push('\n');
        }
        if self.truncated {
            text.push_str(
                "[Output truncated to the retained tail; earlier output is not included.]\n",
            );
        }
        text.push_str(&format!(
            "[State: {:?}; exit code: {}; complete: {}]",
            self.handle.state,
            self.exit_code
                .map(|code| code.to_string())
                .unwrap_or_else(|| "unavailable".into()),
            self.complete
        ));
        if let Some(failure) = &self.failure {
            text.push_str(&format!("\n[Execution failed: {failure}]"));
        }
        text
    }
}

/// Raw bytes preserve UTF-8 characters split across process reads. Serialization
/// uses BinaryData's compact Base64 representation rather than a JSON byte array.
#[derive(Clone, Deserialize, Serialize)]
pub(crate) struct Tail {
    bytes: BinaryData,
    max_bytes: usize,
    max_lines: usize,
    pub(crate) total_bytes: u64,
    pub(crate) truncated: bool,
}

impl Tail {
    pub(crate) fn new(max_bytes: usize, max_lines: usize) -> Self {
        Self {
            bytes: BinaryData::new(Vec::new()),
            max_bytes,
            max_lines,
            total_bytes: 0,
            truncated: false,
        }
    }

    pub(crate) fn push(&mut self, incoming: &[u8]) {
        self.total_bytes = self.total_bytes.saturating_add(incoming.len() as u64);
        let previous = self.bytes.as_slice();
        let keep_incoming = incoming.len().min(self.max_bytes);
        let keep_previous = previous.len().min(self.max_bytes - keep_incoming);
        self.truncated |= keep_incoming < incoming.len() || keep_previous < previous.len();
        let mut bytes = Vec::with_capacity(keep_previous + keep_incoming);
        bytes.extend_from_slice(&previous[previous.len() - keep_previous..]);
        bytes.extend_from_slice(&incoming[incoming.len() - keep_incoming..]);
        let mut lines = usize::from(!bytes.is_empty() && !bytes.ends_with(b"\n"));
        let mut start = 0;
        for (index, byte) in bytes.iter().enumerate().rev() {
            if *byte == b'\n' {
                lines += 1;
                if lines > self.max_lines {
                    start = index + 1;
                    break;
                }
            }
        }
        self.truncated |= start > 0;
        if start > 0 {
            bytes.drain(..start);
        }
        self.bytes = BinaryData::new(bytes);
    }

    pub(crate) fn text(&self) -> String {
        let mut bytes = self.bytes.as_slice();
        // A byte cap may cut through a UTF-8 character; don't invent a leading
        // replacement character for that discarded prefix.
        if self.truncated {
            while bytes.first().is_some_and(|byte| byte & 0xc0 == 0x80) {
                bytes = &bytes[1..];
            }
        }
        String::from_utf8_lossy(bytes).into_owned()
    }
}

#[cfg(test)]
mod tests {
    use super::Tail;

    #[test]
    fn preserves_split_utf8_and_trailing_newline() {
        let mut tail = Tail::new(100, 2);
        for byte in "é\n🙂\n".as_bytes() {
            tail.push(&[*byte]);
        }
        assert_eq!(tail.text(), "é\n🙂\n");
        assert!(!tail.truncated);
    }

    #[test]
    fn limits_lines_and_bytes_including_a_single_long_line() {
        let mut tail = Tail::new(100, 2);
        tail.push(b"a\nb\nc\n");
        assert_eq!(tail.text(), "b\nc\n");
        tail.push(b"d");
        assert_eq!(tail.text(), "c\nd");
        assert!(tail.truncated);
        let mut tail = Tail::new(3, 2);
        tail.push("a🙂x".as_bytes());
        assert_eq!(tail.text(), "x");
        assert_eq!(tail.total_bytes, 6);
    }
}
