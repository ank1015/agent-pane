pub(crate) fn generate(append: Option<&str>) -> String {
    let mut prompt = include_str!("system_prompt.md").to_owned();
    if let Some(append) = append {
        prompt.push_str("\nAdditional session instructions:\n");
        prompt.push_str(append);
    }
    prompt
}
