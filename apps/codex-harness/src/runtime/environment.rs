use agent_contracts::SessionMessage;
use execution_contracts::PathConvention;
use execution_runtime::ExecutionRuntime;
use llm_contracts::{ContentPart, Message, MessageId, TextContent, Timestamp, UserMessage};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use url::Url;
#[cfg(test)]
use uuid::Uuid;

use super::CodexExecutionTarget;

const CODEX_ENVIRONMENT_METADATA_KEY: &str = "codex_environment";
const CODEX_ENVIRONMENT_METADATA_VERSION: u32 = 1;
const CODEX_ENVIRONMENT_MESSAGE_PREFIX: &str = "codex-environment-";

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct CodexEnvironmentSnapshot {
    pub cwd: String,
    pub shell: Option<String>,
    pub current_date: String,
    pub timezone: String,
    pub workspace_roots: Vec<String>,
    pub captured_at: Timestamp,
}

/// Model-visible placement for an append-only environment history item.
///
/// Agent owns transcript revision assignment, so an environment item captured
/// for a new user necessarily commits after that user. The placement anchor lets
/// request formation restore Codex's logical ordering without duplicating or
/// mutating canonical session messages.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub(crate) enum CodexEnvironmentPlacement {
    BeforeMessage { message_id: MessageId },
    AfterMessage { message_id: MessageId },
    BeforeCompaction,
    End,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct CodexEnvironmentMessageMetadata {
    version: u32,
    snapshot: CodexEnvironmentSnapshot,
    placement: CodexEnvironmentPlacement,
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

    #[cfg(test)]
    pub(crate) fn as_message(&self, session_id: Uuid) -> Message {
        self.as_persisted_message(
            MessageId::new(format!("{CODEX_ENVIRONMENT_MESSAGE_PREFIX}{session_id}"))
                .expect("UUID-derived message ID is non-empty"),
            CodexEnvironmentPlacement::End,
            /*previous*/ None,
            /*force_full*/ true,
        )
        .expect("forced full environment always produces a message")
    }

    /// Creates a durable environment history item when state changed, or when a
    /// new context window requires full reinjection.
    pub(crate) fn as_persisted_message(
        &self,
        id: MessageId,
        placement: CodexEnvironmentPlacement,
        previous: Option<&Self>,
        force_full: bool,
    ) -> Option<Message> {
        if !force_full && previous.is_some_and(|previous| self.same_values(previous)) {
            return None;
        }
        let content = match previous {
            Some(previous) if !force_full => render_environment_context_diff(previous, self),
            Some(_) | None => render_environment_context(self),
        };
        let metadata = CodexEnvironmentMessageMetadata {
            version: CODEX_ENVIRONMENT_METADATA_VERSION,
            snapshot: self.clone(),
            placement,
        };
        let Value::Object(metadata) = serde_json::to_value(metadata)
            .expect("environment metadata contains only serializable values")
        else {
            unreachable!("environment metadata serializes as an object")
        };
        Some(Message::User(UserMessage {
            id,
            timestamp: self.captured_at,
            content: vec![ContentPart::Text(TextContent {
                content,
                metadata: Some(serde_json::Map::from_iter([(
                    CODEX_ENVIRONMENT_METADATA_KEY.to_owned(),
                    Value::Object(metadata),
                )])),
            })],
        }))
    }

    fn same_values(&self, other: &Self) -> bool {
        self.cwd == other.cwd
            && self.shell == other.shell
            && self.current_date == other.current_date
            && self.timezone == other.timezone
            && self.workspace_roots == other.workspace_roots
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

/// Renders the model-visible delta used after the initial environment snapshot.
/// Codex repeats the current date/timezone/filesystem envelope whenever any
/// environment value changes, but only repeats cwd/shell when that environment
/// itself changed.
#[must_use]
pub(crate) fn render_environment_context_diff(
    previous: &CodexEnvironmentSnapshot,
    current: &CodexEnvironmentSnapshot,
) -> String {
    let mut rendered = String::from("<environment_context>\n");
    if previous.cwd != current.cwd || previous.shell != current.shell {
        push_element(&mut rendered, "cwd", &current.cwd, "  ");
        if let Some(shell) = &current.shell {
            push_element(&mut rendered, "shell", shell, "  ");
        }
    }
    push_element(&mut rendered, "current_date", &current.current_date, "  ");
    push_element(&mut rendered, "timezone", &current.timezone, "  ");
    rendered.push_str("  <filesystem><workspace_roots>");
    for root in &current.workspace_roots {
        rendered.push_str("<root>");
        push_xml_escaped(&mut rendered, root);
        rendered.push_str("</root>");
    }
    rendered.push_str("</workspace_roots></filesystem>\n</environment_context>");
    rendered
}

#[must_use]
pub(crate) fn latest_environment_snapshot(
    messages: &[SessionMessage],
) -> Option<CodexEnvironmentSnapshot> {
    messages
        .iter()
        .filter_map(|message| {
            environment_message_metadata(&message.message)
                .map(|metadata| (message.revision, metadata.snapshot))
        })
        .max_by_key(|(revision, _)| *revision)
        .map(|(_, snapshot)| snapshot)
}

#[must_use]
pub(crate) fn is_environment_message(message: &Message) -> bool {
    environment_message_metadata(message).is_some()
}

/// Restores logical model order for environment items committed to an
/// append-only transcript after their anchor message.
pub(crate) fn reorder_environment_messages(messages: &mut Vec<Message>) {
    let mut environment_messages = Vec::new();
    let mut ordinary_messages = Vec::with_capacity(messages.len());
    for message in messages.drain(..) {
        if let Some(metadata) = environment_message_metadata(&message) {
            environment_messages.push((message, metadata.placement));
        } else {
            ordinary_messages.push(message);
        }
    }

    for (message, placement) in environment_messages {
        let insertion_index = match placement {
            CodexEnvironmentPlacement::BeforeMessage { message_id } => ordinary_messages
                .iter()
                .position(|message| message_id_of(message) == &message_id)
                .or_else(|| last_compaction_index(&ordinary_messages)),
            CodexEnvironmentPlacement::AfterMessage { message_id } => ordinary_messages
                .iter()
                .position(|message| message_id_of(message) == &message_id)
                .map(|index| index + 1),
            CodexEnvironmentPlacement::BeforeCompaction => {
                last_compaction_index(&ordinary_messages)
            }
            CodexEnvironmentPlacement::End => None,
        }
        .unwrap_or(ordinary_messages.len());
        ordinary_messages.insert(insertion_index, message);
    }
    *messages = ordinary_messages;
}

fn environment_message_metadata(message: &Message) -> Option<CodexEnvironmentMessageMetadata> {
    let Message::User(message) = message else {
        return None;
    };
    if !message
        .id
        .as_str()
        .starts_with(CODEX_ENVIRONMENT_MESSAGE_PREFIX)
    {
        return None;
    }
    let ContentPart::Text(text) = message.content.first()? else {
        return None;
    };
    let metadata = text
        .metadata
        .as_ref()?
        .get(CODEX_ENVIRONMENT_METADATA_KEY)?;
    let metadata =
        serde_json::from_value::<CodexEnvironmentMessageMetadata>(metadata.clone()).ok()?;
    (metadata.version == CODEX_ENVIRONMENT_METADATA_VERSION).then_some(metadata)
}

fn message_id_of(message: &Message) -> &MessageId {
    match message {
        Message::User(message) => &message.id,
        Message::System(message) => &message.id,
        Message::ToolResult(message) => &message.id,
        Message::Assistant(message) => &message.id,
        Message::Custom(message) => &message.id,
    }
}

fn last_compaction_index(messages: &[Message]) -> Option<usize> {
    messages.iter().rposition(|message| {
        let Message::Custom(message) = message else {
            return false;
        };
        message
            .content
            .get("items")
            .and_then(Value::as_array)
            .is_some_and(|items| {
                items.iter().any(|item| {
                    matches!(
                        item.get("type").and_then(Value::as_str),
                        Some("compaction" | "compaction_summary")
                    )
                })
            })
    })
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
    use llm_contracts::{ContentPart, Message, MessageId, TextContent, Timestamp, UserMessage};

    use super::{
        CodexEnvironmentPlacement, CodexEnvironmentSnapshot, is_environment_message,
        render_environment_context, reorder_environment_messages,
    };

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

    #[test]
    fn persists_only_environment_changes_and_renders_a_context_delta() {
        let initial = snapshot("/work/repo", "2026-08-30", Timestamp(1));
        let unchanged_at_a_later_capture = snapshot("/work/repo", "2026-08-30", Timestamp(2));
        assert!(
            unchanged_at_a_later_capture
                .as_persisted_message(
                    MessageId::new("codex-environment-unchanged").expect("message ID"),
                    CodexEnvironmentPlacement::End,
                    Some(&initial),
                    false,
                )
                .is_none(),
            "capture time alone must not invalidate the stable prompt prefix"
        );

        let next_day = snapshot("/work/repo", "2026-08-31", Timestamp(3));
        let Message::User(update) = next_day
            .as_persisted_message(
                MessageId::new("codex-environment-next-day").expect("message ID"),
                CodexEnvironmentPlacement::End,
                Some(&initial),
                false,
            )
            .expect("changed environment should be persisted")
        else {
            panic!("environment context is a user message")
        };
        let ContentPart::Text(update) = &update.content[0] else {
            panic!("environment context is text")
        };
        assert!(!update.content.contains("<cwd>"));
        assert!(!update.content.contains("<shell>"));
        assert!(
            update
                .content
                .contains("<current_date>2026-08-31</current_date>")
        );
        assert!(update.content.contains("<filesystem>"));
    }

    #[test]
    fn restores_append_only_environment_items_before_their_user_turns() {
        let first_user = text_user("user-1", "first");
        let second_user = text_user("user-2", "second");
        let first_environment = snapshot("/work/repo", "2026-08-30", Timestamp(1))
            .as_persisted_message(
                MessageId::new("codex-environment-first").expect("message ID"),
                CodexEnvironmentPlacement::BeforeMessage {
                    message_id: MessageId::new("user-1").expect("message ID"),
                },
                None,
                true,
            )
            .expect("full environment");
        let second_environment = snapshot("/work/other", "2026-08-30", Timestamp(3))
            .as_persisted_message(
                MessageId::new("codex-environment-second").expect("message ID"),
                CodexEnvironmentPlacement::BeforeMessage {
                    message_id: MessageId::new("user-2").expect("message ID"),
                },
                Some(&snapshot("/work/repo", "2026-08-30", Timestamp(1))),
                false,
            )
            .expect("environment delta");

        // This is canonical commit order: each environment item had to be
        // appended after the already-committed external user message.
        let mut messages = vec![
            first_user,
            first_environment,
            second_user,
            second_environment,
        ];
        reorder_environment_messages(&mut messages);

        assert!(is_environment_message(&messages[0]));
        assert_eq!(message_id(&messages[1]), "user-1");
        assert!(is_environment_message(&messages[2]));
        assert_eq!(message_id(&messages[3]), "user-2");
    }

    fn snapshot(cwd: &str, current_date: &str, captured_at: Timestamp) -> CodexEnvironmentSnapshot {
        CodexEnvironmentSnapshot::new(
            cwd,
            Some("zsh".to_owned()),
            current_date,
            "Asia/Kolkata",
            vec!["/work".to_owned()],
            captured_at,
        )
        .expect("valid snapshot")
    }

    fn text_user(id: &str, content: &str) -> Message {
        Message::User(UserMessage {
            id: MessageId::new(id).expect("message ID"),
            timestamp: Timestamp(1),
            content: vec![ContentPart::Text(TextContent {
                content: content.to_owned(),
                metadata: None,
            })],
        })
    }

    fn message_id(message: &Message) -> &str {
        match message {
            Message::User(message) => message.id.as_str(),
            Message::System(message) => message.id.as_str(),
            Message::ToolResult(message) => message.id.as_str(),
            Message::Assistant(message) => message.id.as_str(),
            Message::Custom(message) => message.id.as_str(),
        }
    }
}
