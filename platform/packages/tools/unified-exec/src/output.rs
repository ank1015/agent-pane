use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

const APPROX_BYTES_PER_TOKEN: usize = 4;

/// Shared structured output for `exec_command` and `write_stdin`.
#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct UnifiedExecOutput {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub chunk_id: Option<String>,
    pub wall_time_seconds: f64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub exit_code: Option<i32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub session_id: Option<i32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub original_token_count: Option<usize>,
    /// Bounded raw output. Use `code_mode_result` or `to_text` for delivery.
    pub output: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) max_output_tokens: Option<usize>,
    pub(crate) model_output_tokens: usize,
    pub(crate) history_output_tokens: usize,
    #[serde(default)]
    pub(crate) omitted_bytes: u64,
}

impl UnifiedExecOutput {
    #[must_use]
    pub fn to_text(&self) -> String {
        let mut sections = Vec::new();
        if let Some(chunk_id) = &self.chunk_id {
            sections.push(format!("Chunk ID: {chunk_id}"));
        }
        sections.push(format!("Wall time: {:.4} seconds", self.wall_time_seconds));
        if let Some(exit_code) = self.exit_code {
            sections.push(format!("Process exited with code {exit_code}"));
        }
        if let Some(session_id) = self.session_id {
            sections.push(format!("Process running with session ID {session_id}"));
        }
        if let Some(tokens) = self.original_token_count {
            sections.push(format!("Original token count: {tokens}"));
        }
        sections.push("Output:".to_string());
        let mut result = sections.join("\n");
        result.push('\n');
        // Reserve metadata space in the history serialization budget, matching
        // Codex's 1.2 multiplier, so history need not truncate output twice.
        let history_bytes = self
            .history_output_tokens
            .saturating_mul(4)
            .saturating_add(self.history_output_tokens.saturating_mul(4) / 5);
        let available = history_bytes.saturating_sub(result.len());
        let mut budget = self.model_output_tokens;
        let mut output = self.truncated_output(budget);
        while output.len() > available && budget > 0 {
            budget = budget.saturating_sub(approximate_tokens((output.len() - available) as u64));
            output = self.truncated_output(budget);
        }
        result.push_str(&output);
        result
    }

    #[must_use]
    pub fn code_mode_result(&self) -> Value {
        let mut result = json!(self);
        let fields = result.as_object_mut().expect("output object");
        fields.remove("max_output_tokens");
        fields.remove("model_output_tokens");
        fields.remove("history_output_tokens");
        fields.remove("omitted_bytes");
        if let Some(budget) = self.max_output_tokens {
            fields.insert("output".into(), json!(self.truncated_output(budget)));
        }
        result
    }
    fn truncated_output(&self, budget: usize) -> String {
        if self.omitted_bytes == 0 {
            return formatted_truncate(&self.output, budget);
        }
        let marker = format!("... {} bytes omitted ...", self.omitted_bytes);
        if self.output.len() <= budget.saturating_mul(4) {
            return if self.output.contains(&marker) {
                self.output.clone()
            } else {
                format!("{marker}\n{}", self.output)
            };
        }
        let truncated = truncate_to_tokens(&self.output, budget);
        let notice = if truncated.contains(&marker) {
            String::new()
        } else {
            format!("{marker}\n")
        };
        format!(
            "Warning: truncated output (original token count: {})\n{notice}\n{truncated}",
            self.original_token_count
                .unwrap_or_else(|| approximate_tokens(self.output.len() as u64))
        )
    }
}

fn formatted_truncate(text: &str, budget: usize) -> String {
    if text.len() <= budget.saturating_mul(APPROX_BYTES_PER_TOKEN) {
        return text.to_owned();
    }
    format!(
        "Warning: truncated output (original token count: {})\nTotal output lines: {}\n\n{}",
        approximate_tokens(text.len() as u64),
        text.lines().count(),
        truncate_to_tokens(text, budget)
    )
}

pub(crate) fn approximate_tokens(bytes: u64) -> usize {
    let bytes = usize::try_from(bytes).unwrap_or(usize::MAX);
    bytes.saturating_add(APPROX_BYTES_PER_TOKEN - 1) / APPROX_BYTES_PER_TOKEN
}

pub(crate) fn truncate_to_tokens(text: &str, max_tokens: usize) -> String {
    let max_bytes = max_tokens.saturating_mul(APPROX_BYTES_PER_TOKEN);
    if !text.is_empty() && text.len() <= max_bytes {
        return text.to_string();
    }
    if text.is_empty() {
        return String::new();
    }

    let left_budget = max_bytes / 2;
    let right_budget = max_bytes.saturating_sub(left_budget);
    let mut prefix_end = 0;
    for (index, character) in text.char_indices() {
        let end = index + character.len_utf8();
        if end > left_budget {
            break;
        }
        prefix_end = end;
    }
    let target = text.len().saturating_sub(right_budget);
    let suffix_start = text
        .char_indices()
        .map(|(index, _)| index)
        .find(|index| *index >= target)
        .unwrap_or(text.len())
        .max(prefix_end);
    let removed = text.len().saturating_sub(max_bytes);
    let removed_tokens = approximate_tokens(u64::try_from(removed).unwrap_or(u64::MAX));
    format!(
        "{}…{removed_tokens} tokens truncated…{}",
        &text[..prefix_end],
        &text[suffix_start..]
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn codex_truncation_fixtures_and_omission_notice_survive_small_budgets() {
        assert_eq!(
            formatted_truncate("abcdefghijklmnop", 2),
            "Warning: truncated output (original token count: 4)\nTotal output lines: 1\n\nabcd…2 tokens truncated…mnop"
        );
        // Count removed tokens by byte budget, not rounded UTF-8 slice lengths.
        assert_eq!(truncate_to_tokens("🙂🙂🙂", 1), "…2 tokens truncated…");
        assert_eq!(truncate_to_tokens("hello", 0), "…2 tokens truncated…");
        let output = UnifiedExecOutput {
            chunk_id: Some("abcdef".into()),
            wall_time_seconds: 0.0,
            exit_code: Some(0),
            session_id: None,
            original_token_count: Some(250),
            output: "start\n... 990 bytes omitted ...\nfinish".into(),
            max_output_tokens: Some(1),
            model_output_tokens: 1,
            history_output_tokens: 1000,
            omitted_bytes: 990,
        };
        assert!(output.to_text().contains("990 bytes omitted"));
        assert!(
            output.code_mode_result()["output"]
                .as_str()
                .unwrap()
                .contains("990 bytes omitted")
        );
        assert!(output.code_mode_result().get("omitted_bytes").is_none());
    }

    #[test]
    fn renders_codex_header_and_structured_result() {
        let output = UnifiedExecOutput {
            chunk_id: Some("abc123".into()),
            wall_time_seconds: 1.25,
            exit_code: None,
            session_id: Some(42),
            original_token_count: Some(10),
            output: "hello".into(),
            max_output_tokens: None,
            model_output_tokens: 10_000,
            history_output_tokens: 10_000,
            omitted_bytes: 0,
        };
        assert_eq!(
            output.to_text(),
            "Chunk ID: abc123\nWall time: 1.2500 seconds\nProcess running with session ID 42\nOriginal token count: 10\nOutput:\nhello"
        );
        assert_eq!(output.code_mode_result()["session_id"], 42);
    }

    #[test]
    fn token_truncation_preserves_utf8_boundaries_and_both_ends() {
        let output = truncate_to_tokens("start🙂middle🙂finish", 3);
        assert!(output.starts_with("start"));
        assert!(output.ends_with("finish"));
        assert!(output.contains("tokens truncated"));
    }
}
