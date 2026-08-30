/// Base instructions shipped for Sol, Terra, and Luna by the inspected Codex
/// model catalog. The three entries currently use this identical template.
pub const BASE_INSTRUCTIONS: &str = include_str!("prompts/base_instructions.md");

#[must_use]
pub fn generate_instructions(external_prompt: Option<&str>, is_replaced: bool) -> String {
    match external_prompt {
        Some(prompt) if is_replaced => prompt.to_owned(),
        Some(prompt) => format!("{BASE_INSTRUCTIONS}\n\n{prompt}"),
        None => BASE_INSTRUCTIONS.to_owned(),
    }
}

#[cfg(test)]
mod tests {
    use super::{BASE_INSTRUCTIONS, generate_instructions};

    #[test]
    fn uses_the_codex_catalog_instructions_by_default() {
        assert!(BASE_INSTRUCTIONS.starts_with("You are Codex, an agent based on GPT-5."));
        assert!(BASE_INSTRUCTIONS.contains("# Working with the user"));
        assert!(BASE_INSTRUCTIONS.contains("# Rules for getting work done"));
        assert!(!BASE_INSTRUCTIONS.contains("AGENTS.md"));
        assert_eq!(generate_instructions(None, false), BASE_INSTRUCTIONS);
    }

    #[test]
    fn supports_the_same_append_and_replace_contract_as_pi() {
        assert_eq!(
            generate_instructions(Some("Project-specific rule."), false),
            format!("{BASE_INSTRUCTIONS}\n\nProject-specific rule.")
        );
        assert_eq!(
            generate_instructions(Some("Replacement instructions."), true),
            "Replacement instructions."
        );
    }
}
