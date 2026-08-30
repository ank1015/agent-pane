use execution_contracts::PathConvention;
use execution_runtime::ExecutionRuntime;
use llm_contracts::{ContentPart, Message, MessageId, TextContent, Timestamp, UserMessage};
use url::Url;
use uuid::Uuid;

use super::CodexExecutionTarget;

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CodexEnvironmentSnapshot {
    pub cwd: String,
    pub shell: Option<String>,
    pub current_date: String,
    pub timezone: String,
    pub workspace_roots: Vec<String>,
    pub captured_at: Timestamp,
}

impl CodexEnvironmentSnapshot {
    pub fn new(
        cwd: impl Into<String>,
        shell: Option<String>,
        current_date: impl Into<String>,
        timezone: impl Into<String>,
        workspace_roots: Vec<String>,
        captured_at: Timestamp,
    ) -> Result<Self, CodexEnvironmentError> {
        let snapshot = Self {
            cwd: cwd.into(),
            shell,
            current_date: current_date.into(),
            timezone: timezone.into(),
            workspace_roots,
            captured_at,
        };
        snapshot.validate()?;
        Ok(snapshot)
    }

    /// Resolves display paths and the target shell from the execution runtime's
    /// immutable descriptor. Date and timezone remain explicit inputs because
    /// the execution interface does not currently expose a target clock.
    pub fn from_execution_runtime(
        runtime: &dyn ExecutionRuntime,
        target: &CodexExecutionTarget,
        current_date: impl Into<String>,
        timezone: impl Into<String>,
        captured_at: Timestamp,
    ) -> Result<Self, CodexEnvironmentError> {
        let descriptor = runtime.descriptor();
        if descriptor.machine_id != target.machine_id {
            return Err(CodexEnvironmentError::MachineMismatch {
                expected: target.machine_id.to_string(),
                actual: descriptor.machine_id.to_string(),
            });
        }
        let active_root = descriptor
            .workspace_roots
            .iter()
            .find(|root| root.id == target.workspace_root_id)
            .ok_or_else(|| {
                CodexEnvironmentError::UnknownWorkspaceRoot(target.workspace_root_id.to_string())
            })?;
        let workspace_roots = descriptor
            .workspace_roots
            .iter()
            .map(|root| native_path(&root.uri, descriptor.path_convention))
            .collect::<Result<Vec<_>, _>>()?;
        let root = native_path(&active_root.uri, descriptor.path_convention)?;
        let cwd = join_workspace_path(&root, &target.cwd, descriptor.path_convention);
        let shell = descriptor
            .default_shell
            .as_ref()
            .map(|shell| shell.name.clone());
        Self::new(
            cwd,
            shell,
            current_date,
            timezone,
            workspace_roots,
            captured_at,
        )
    }

    fn validate(&self) -> Result<(), CodexEnvironmentError> {
        for (field, value) in [
            ("cwd", self.cwd.as_str()),
            ("current_date", self.current_date.as_str()),
            ("timezone", self.timezone.as_str()),
        ] {
            if value.trim().is_empty() {
                return Err(CodexEnvironmentError::EmptyField(field));
            }
        }
        if self.workspace_roots.is_empty() {
            return Err(CodexEnvironmentError::MissingWorkspaceRoots);
        }
        if self
            .workspace_roots
            .iter()
            .any(|root| root.trim().is_empty())
        {
            return Err(CodexEnvironmentError::EmptyWorkspaceRoot);
        }
        if self
            .shell
            .as_ref()
            .is_some_and(|shell| shell.trim().is_empty())
        {
            return Err(CodexEnvironmentError::EmptyField("shell"));
        }
        Ok(())
    }

    pub(crate) fn as_message(&self, session_id: Uuid) -> Message {
        Message::User(UserMessage {
            id: MessageId::new(format!("codex-environment-{session_id}"))
                .expect("UUID-derived message ID is non-empty"),
            timestamp: self.captured_at,
            content: vec![ContentPart::Text(TextContent {
                content: render_environment_context(self),
                metadata: None,
            })],
        })
    }
}

#[must_use]
pub fn render_environment_context(snapshot: &CodexEnvironmentSnapshot) -> String {
    let mut rendered = String::from("<environment_context>\n");
    push_element(&mut rendered, "cwd", &snapshot.cwd, "  ");
    if let Some(shell) = &snapshot.shell {
        push_element(&mut rendered, "shell", shell, "  ");
    }
    push_element(&mut rendered, "current_date", &snapshot.current_date, "  ");
    push_element(&mut rendered, "timezone", &snapshot.timezone, "  ");
    rendered.push_str("  <filesystem><workspace_roots>");
    for root in &snapshot.workspace_roots {
        rendered.push_str("<root>");
        push_xml_escaped(&mut rendered, root);
        rendered.push_str("</root>");
    }
    rendered.push_str("</workspace_roots></filesystem>\n</environment_context>");
    rendered
}

fn push_element(rendered: &mut String, name: &str, value: &str, indent: &str) {
    rendered.push_str(indent);
    rendered.push('<');
    rendered.push_str(name);
    rendered.push('>');
    push_xml_escaped(rendered, value);
    rendered.push_str("</");
    rendered.push_str(name);
    rendered.push_str(">\n");
}

fn push_xml_escaped(rendered: &mut String, value: &str) {
    for character in value.chars() {
        match character {
            '&' => rendered.push_str("&amp;"),
            '<' => rendered.push_str("&lt;"),
            '>' => rendered.push_str("&gt;"),
            '"' => rendered.push_str("&quot;"),
            '\'' => rendered.push_str("&apos;"),
            character => rendered.push(character),
        }
    }
}

fn native_path(uri: &str, convention: PathConvention) -> Result<String, CodexEnvironmentError> {
    let url = Url::parse(uri)
        .map_err(|_| CodexEnvironmentError::InvalidWorkspaceRootUri(uri.to_owned()))?;
    if url.scheme() != "file" {
        return Err(CodexEnvironmentError::InvalidWorkspaceRootUri(
            uri.to_owned(),
        ));
    }
    let path = url
        .to_file_path()
        .map_err(|()| CodexEnvironmentError::InvalidWorkspaceRootUri(uri.to_owned()))?
        .to_string_lossy()
        .into_owned();
    Ok(match convention {
        PathConvention::Posix if path == "/" => path,
        PathConvention::Posix => path.trim_end_matches('/').to_owned(),
        PathConvention::Windows => {
            let path = path.trim_start_matches('/').replace('/', "\\");
            if path.len() == 3 && path.as_bytes()[1] == b':' && path.ends_with('\\') {
                path
            } else {
                path.trim_end_matches('\\').to_owned()
            }
        }
    })
}

fn join_workspace_path(root: &str, relative: &str, convention: PathConvention) -> String {
    if relative == "." {
        return root.to_owned();
    }
    let separator = match convention {
        PathConvention::Posix => '/',
        PathConvention::Windows => '\\',
    };
    format!(
        "{}{}{}",
        root.trim_end_matches(['/', '\\']),
        separator,
        relative.replace('/', &separator.to_string())
    )
}

#[derive(Debug, Eq, PartialEq, thiserror::Error)]
pub enum CodexEnvironmentError {
    #[error("execution runtime machine mismatch: expected {expected:?}, got {actual:?}")]
    MachineMismatch { expected: String, actual: String },
    #[error("execution runtime does not expose workspace root {0:?}")]
    UnknownWorkspaceRoot(String),
    #[error("workspace root URI {0:?} is not a convertible absolute file URI")]
    InvalidWorkspaceRootUri(String),
    #[error("environment field {0:?} must not be empty")]
    EmptyField(&'static str),
    #[error("environment must contain at least one workspace root")]
    MissingWorkspaceRoots,
    #[error("environment workspace roots must not contain an empty path")]
    EmptyWorkspaceRoot,
}

#[cfg(test)]
mod tests {
    use llm_contracts::Timestamp;

    use super::{CodexEnvironmentSnapshot, render_environment_context};

    #[test]
    fn renders_codex_style_environment_without_permission_or_sandbox_context() {
        let snapshot = CodexEnvironmentSnapshot::new(
            "/work/repo<&>",
            Some("zsh".to_owned()),
            "2026-08-30",
            "Asia/Kolkata",
            vec!["/work/repo<&>".to_owned(), "/work/other".to_owned()],
            Timestamp(42),
        )
        .expect("valid snapshot");
        let rendered = render_environment_context(&snapshot);
        assert_eq!(
            rendered,
            "<environment_context>\n  <cwd>/work/repo&lt;&amp;&gt;</cwd>\n  <shell>zsh</shell>\n  <current_date>2026-08-30</current_date>\n  <timezone>Asia/Kolkata</timezone>\n  <filesystem><workspace_roots><root>/work/repo&lt;&amp;&gt;</root><root>/work/other</root></workspace_roots></filesystem>\n</environment_context>"
        );
        assert!(!rendered.contains("permission"));
        assert!(!rendered.contains("sandbox"));
    }
}
