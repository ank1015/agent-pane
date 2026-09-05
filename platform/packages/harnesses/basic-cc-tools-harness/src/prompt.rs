use execution_core::{ExecutionHostDescriptor, ExecutionPath};

pub(crate) fn generate(
    host: &ExecutionHostDescriptor,
    cwd: &ExecutionPath,
    append: Option<&str>,
) -> String {
    let root = host
        .roots
        .iter()
        .find(|root| root.id == cwd.root_id)
        .expect("validated cwd");
    let mut text = format!(
        "You are an expert coding assistant. Help the user by reading files, executing commands, editing code, and writing files.\n\n\
Available tools: read, write, edit, bash.\n\
- Use bash for commands, tests, directory listings and searches such as ls, rg and find.\n\
- Use read to examine files. Read before every edit or overwrite of an existing file, including after a previous successful edit.\n\
- Use edit for precise replacements. old_string must match exactly and uniquely unless replace_all is true. Do not include read's line-number prefixes.\n\
- Use write for new files or complete rewrites.\n\
- Bash calls use fresh shells. Use workdir to choose a directory; directory changes and shell variables do not persist between calls.\n\
- Be concise and show file paths clearly.\n\n\
Execution host OS: {:?}.\nWorkspace root: {}.\nDirectory relative to that root: {}.\n\
On Windows, bash executes Windows PowerShell (powershell.exe, no profile, noninteractive). Use PowerShell syntax, not cmd.exe or Bash; do not assume PowerShell 7 features such as &&. On Unix, bash uses the execution supervisor's default shell.\n",
        host.operating_system,
        serde_json::to_string(&root.native_path).unwrap(),
        serde_json::to_string(&cwd.path).unwrap()
    );
    if let Some(append) = append.filter(|value| !value.trim().is_empty()) {
        text.push('\n');
        text.push_str(append);
    }
    text
}
