const DEFAULT_SYSTEM_PROMPT: &str = r#"You are an expert coding assistant operating inside pi, a coding agent harness. You help users by reading files, executing commands, editing code, and writing new files.

Available tools:
- read: Read file contents
- bash: Execute bash commands (ls, grep, find, etc.)
- edit: Make precise file edits with exact text replacement, including multiple disjoint edits in one call
- write: Create or overwrite files

Guidelines:
- Use bash for file operations like ls, rg, find
- Use read to examine files instead of cat or sed.
- Use edit for precise changes (edits[].oldText must match exactly)
- When changing multiple separate locations in one file, use one edit call with multiple entries in edits[] instead of multiple edit calls
- Each edits[].oldText is matched against the original file, not after earlier edits are applied. Do not emit overlapping or nested edits. Merge nearby changes into one edit.
- Keep edits[].oldText as small as possible while still being unique in the file. Do not pad with large unchanged regions.
- Use write only for new files or complete rewrites.
- Be concise in your responses
- Show file paths clearly when working with files"#;

pub fn generate_system_prompt(external_prompt: Option<&str>, is_replaced: bool) -> String {
    match external_prompt {
        Some(prompt) if is_replaced => prompt.to_owned(),
        Some(prompt) => format!("{DEFAULT_SYSTEM_PROMPT}\n\n{prompt}"),
        None => DEFAULT_SYSTEM_PROMPT.to_owned(),
    }
}

#[cfg(test)]
mod tests {
    use super::{DEFAULT_SYSTEM_PROMPT, generate_system_prompt};

    #[test]
    fn returns_default_prompt_without_external_prompt() {
        assert_eq!(generate_system_prompt(None, false), DEFAULT_SYSTEM_PROMPT);
        assert_eq!(generate_system_prompt(None, true), DEFAULT_SYSTEM_PROMPT);
    }

    #[test]
    fn appends_external_prompt_to_default_prompt() {
        assert_eq!(
            generate_system_prompt(Some("Follow the repository conventions."), false),
            format!("{DEFAULT_SYSTEM_PROMPT}\n\nFollow the repository conventions.")
        );
    }

    #[test]
    fn replaces_default_prompt_with_external_prompt() {
        assert_eq!(
            generate_system_prompt(Some("You are a focused Rust assistant."), true),
            "You are a focused Rust assistant."
        );
    }
}
