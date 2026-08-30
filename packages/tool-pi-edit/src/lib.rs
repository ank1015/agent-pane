//! Reusable implementation of Pi's exact text `edit` tool.
//!
//! The crate owns the model-facing definition, argument validation, complete
//! file read, exact replacement logic, and revision-fenced mutation. Harnesses
//! remain responsible for selecting the tool and recording its result.

use execution_contracts::{
    ApplyMutationRequest, ContentSource, ContinuationCursor, FileRevision, MachineDescriptor,
    MutationAtomicity, MutationOperation, MutationPlan, MutationPostActions, OperationId,
    PathConvention, PathSpec, ReadMode, ReadRequest, ReadResult, TextPageRequest, WorkspaceRootId,
};
use execution_runtime::{ExecutionRuntime, OperationContext};
use llm_contracts::{ContentPart, FunctionTool, TextContent, ToolArguments, ToolDefinition};
use serde::{Deserialize, Serialize, de::DeserializeOwned};
use serde_json::{Value, json};
use url::Url;
use uuid::Uuid;

/// Name used in the model-facing tool definition.
pub const TOOL_NAME: &str = "edit";
/// Maximum lines requested per page while loading a file for editing.
pub const EDIT_PAGE_LINES: u32 = 100_000;
/// Maximum bytes requested per page and per line while loading a file.
pub const EDIT_PAGE_BYTES: u64 = 8 * 1_024 * 1_024;

/// Typed arguments accepted by the edit tool.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct EditArguments {
    pub path: String,
    pub edits: Vec<EditInput>,
}

/// One exact replacement matched against the original file.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct EditInput {
    pub old_text: String,
    pub new_text: String,
}

/// Execution inputs specific to the edit tool.
pub struct EditToolContext<'a> {
    runtime: &'a dyn ExecutionRuntime,
    operation: &'a OperationContext,
    workspace_root_id: WorkspaceRootId,
    cwd: String,
}

impl<'a> EditToolContext<'a> {
    /// Creates an edit context rooted at `workspace_root_id` and `cwd`.
    pub fn new(
        runtime: &'a dyn ExecutionRuntime,
        operation: &'a OperationContext,
        workspace_root_id: WorkspaceRootId,
        cwd: impl Into<String>,
    ) -> Result<Self, EditToolError> {
        let cwd = normalize_relative_path(".", &cwd.into())?;
        Ok(Self {
            runtime,
            operation,
            workspace_root_id,
            cwd,
        })
    }

    #[must_use]
    pub fn runtime(&self) -> &dyn ExecutionRuntime {
        self.runtime
    }

    #[must_use]
    pub const fn operation(&self) -> &OperationContext {
        self.operation
    }

    #[must_use]
    pub const fn workspace_root_id(&self) -> &WorkspaceRootId {
        &self.workspace_root_id
    }

    #[must_use]
    pub fn cwd(&self) -> &str {
        &self.cwd
    }

    fn resolve_path(&self, input: &str) -> Result<PathSpec, EditToolError> {
        resolve_path(
            self.runtime.descriptor(),
            &self.workspace_root_id,
            &self.cwd,
            input,
        )
    }
}

/// Successful output from the edit tool.
#[derive(Clone, Debug, PartialEq)]
pub struct EditToolOutput {
    pub content: Vec<ContentPart>,
    pub details: Option<Value>,
}

impl EditToolOutput {
    fn text(content: impl Into<String>) -> Self {
        Self {
            content: vec![ContentPart::Text(TextContent {
                content: content.into(),
                metadata: None,
            })],
            details: None,
        }
    }

    fn with_details(mut self, details: Value) -> Self {
        self.details = Some(details);
        self
    }
}

/// Structured edit failure that a harness can map into its own tool result.
#[derive(Debug, thiserror::Error)]
#[error("{message}")]
pub struct EditToolError {
    name: &'static str,
    message: String,
    details: Option<Value>,
}

impl EditToolError {
    fn new(name: &'static str, message: impl Into<String>) -> Self {
        Self {
            name,
            message: message.into(),
            details: None,
        }
    }

    fn invalid_arguments(message: impl std::fmt::Display) -> Self {
        Self::new(
            "invalid_arguments",
            format!("Invalid arguments for {TOOL_NAME}: {message}"),
        )
    }

    fn invalid_path(message: impl Into<String>) -> Self {
        Self::new("invalid_path", message)
    }

    fn missing_capability(capability: &str) -> Self {
        Self::new(
            "unsupported_capability",
            format!("Execution runtime does not support {capability}"),
        )
    }

    fn process(message: impl Into<String>) -> Self {
        Self::new("process_error", message)
    }

    fn edit(message: impl Into<String>) -> Self {
        Self::new("edit_failed", message)
    }

    #[must_use]
    pub const fn name(&self) -> &'static str {
        self.name
    }

    #[must_use]
    pub fn message(&self) -> &str {
        &self.message
    }

    #[must_use]
    pub const fn details(&self) -> Option<&Value> {
        self.details.as_ref()
    }

    #[must_use]
    pub fn into_parts(self) -> (&'static str, String, Option<Value>) {
        (self.name, self.message, self.details)
    }
}

impl From<execution_contracts::ExecutionError> for EditToolError {
    fn from(error: execution_contracts::ExecutionError) -> Self {
        let details = serde_json::to_value(&error).ok();
        Self {
            name: "execution_error",
            message: error.message,
            details,
        }
    }
}

/// Returns the model-facing Pi edit tool definition.
#[must_use]
pub fn definition() -> ToolDefinition {
    function_tool(
        TOOL_NAME,
        "Edit a single file using exact text replacement. Every edits[].oldText must match a unique, non-overlapping region of the original file. If two changes affect the same block or nearby lines, merge them into one edit instead of emitting overlapping edits. Do not include large unchanged regions just to connect distant changes.",
        json!({
            "type": "object",
            "properties": {
                "path": {
                    "type": "string",
                    "description": "Path to the file to edit (relative or absolute within the current workspace)"
                },
                "edits": {
                    "type": "array",
                    "description": "One or more targeted replacements. Each edit is matched against the original file, not incrementally. Do not include overlapping or nested edits. If two changes touch the same block or nearby lines, merge them into one edit instead.",
                    "items": {
                        "type": "object",
                        "properties": {
                            "oldText": {
                                "type": "string",
                                "description": "Exact text for one targeted replacement. It must be unique in the original file and must not overlap with any other edits[].oldText in the same call."
                            },
                            "newText": {
                                "type": "string",
                                "description": "Replacement text for this targeted edit."
                            }
                        },
                        "required": ["oldText", "newText"]
                    }
                }
            },
            "required": ["path", "edits"]
        }),
    )
}

/// Parses provider-neutral LLM tool arguments into typed edit arguments.
pub fn parse_arguments(arguments: &ToolArguments) -> Result<EditArguments, EditToolError> {
    parse(arguments).map_err(EditToolError::invalid_arguments)
}

/// Parses and executes an LLM tool call's arguments.
pub async fn execute_edit_tool(
    arguments: &ToolArguments,
    context: &EditToolContext<'_>,
) -> Result<EditToolOutput, EditToolError> {
    execute(parse_arguments(arguments)?, context).await
}

/// Executes typed exact replacements against an execution runtime.
pub async fn execute(
    arguments: EditArguments,
    context: &EditToolContext<'_>,
) -> Result<EditToolOutput, EditToolError> {
    if arguments.edits.is_empty() {
        return Err(EditToolError::invalid_arguments(
            "edits must contain at least one replacement",
        ));
    }
    let path = context.resolve_path(&arguments.path)?;
    let (raw_content, revision) = read_complete_text(context, path.clone()).await?;
    let (bom, content) = strip_bom(&raw_content);
    let line_ending = detect_line_ending(content);
    let normalized_content = normalize_line_endings(content);
    let edits = normalize_edits(arguments.edits);
    let mut matches = match_edits(&normalized_content, edits, &arguments.path)?;
    let first_changed_line = matches
        .iter()
        .map(|matched| line_number(&normalized_content, matched.start))
        .min();
    matches.sort_by_key(|matched| matched.start);
    reject_overlaps(&matches, &arguments.path)?;

    let mut new_content = normalized_content.clone();
    for matched in matches.iter().rev() {
        new_content.replace_range(matched.start..matched.end, &matched.new_text);
    }
    if new_content == normalized_content {
        return Err(EditToolError::edit(format!(
            "No changes made to {}. The replacements produced identical content.",
            arguments.path
        )));
    }
    let new_content = restore_line_endings(&new_content, line_ending);
    let final_content = if bom {
        format!("\u{feff}{new_content}")
    } else {
        new_content
    };

    let mutation = context
        .runtime
        .workspace_mutation()
        .ok_or_else(|| EditToolError::missing_capability("workspace mutations"))?;
    let result = mutation
        .apply(
            context.operation,
            ApplyMutationRequest {
                operation_id: operation_id(),
                plan: MutationPlan {
                    operations: vec![MutationOperation::PutFile {
                        path,
                        content: ContentSource::Text {
                            content: final_content,
                        },
                        create_parents: false,
                        preserve_utf8_bom: false,
                        expected_revision: revision,
                    }],
                    atomicity: MutationAtomicity::AtomicIfSupported,
                    post_actions: MutationPostActions::default(),
                },
            },
        )
        .await?;

    Ok(EditToolOutput::text(format!(
        "Successfully replaced {} block(s) in {}.",
        matches.len(),
        arguments.path
    ))
    .with_details(json!({
        "first_changed_line": first_changed_line,
        "replacements": matches.len(),
        "mutation": result
    })))
}

struct MatchedEdit {
    index: usize,
    start: usize,
    end: usize,
    new_text: String,
}

async fn read_complete_text(
    context: &EditToolContext<'_>,
    path: PathSpec,
) -> Result<(String, Option<FileRevision>), EditToolError> {
    let mut content = String::new();
    let mut revision = None;
    let mut start_line = Some(1);
    let mut cursor: Option<ContinuationCursor> = None;
    loop {
        let result = context
            .runtime
            .workspace_query()
            .read(
                context.operation,
                ReadRequest {
                    path: path.clone(),
                    mode: ReadMode::Text,
                    page: Some(TextPageRequest {
                        start_line,
                        cursor: cursor.clone(),
                        max_lines: EDIT_PAGE_LINES,
                        max_bytes: EDIT_PAGE_BYTES,
                        max_line_bytes: EDIT_PAGE_BYTES,
                        include_total_lines: false,
                    }),
                },
            )
            .await?;
        let ReadResult::Text { page } = result else {
            return Err(EditToolError::edit("edit requires a UTF-8 text file"));
        };
        if page.lines_truncated {
            return Err(EditToolError::edit(format!(
                "file contains a line larger than the {EDIT_PAGE_BYTES} byte edit page limit"
            )));
        }
        if let Some(expected) = &revision {
            if page.metadata.revision.as_ref() != Some(expected) {
                return Err(EditToolError::edit("file changed while it was being read"));
            }
        } else {
            revision = page.metadata.revision.clone();
        }
        content.push_str(&page.content);
        if !page.has_more {
            break;
        }
        if let Some(next_cursor) = page.cursor {
            cursor = Some(next_cursor);
            start_line = None;
        } else if let Some(next_line) = page.next_line {
            cursor = None;
            start_line = Some(next_line);
        } else {
            return Err(EditToolError::process(
                "text read was truncated without a continuation",
            ));
        }
    }
    Ok((content, revision))
}

fn normalize_edits(edits: Vec<EditInput>) -> Vec<EditInput> {
    edits
        .into_iter()
        .map(|edit| EditInput {
            old_text: normalize_line_endings(&edit.old_text),
            new_text: normalize_line_endings(&edit.new_text),
        })
        .collect()
}

fn match_edits(
    content: &str,
    edits: Vec<EditInput>,
    path: &str,
) -> Result<Vec<MatchedEdit>, EditToolError> {
    let total = edits.len();
    edits
        .into_iter()
        .enumerate()
        .map(|(index, edit)| {
            if edit.old_text.is_empty() {
                return Err(edit_error(
                    path,
                    index,
                    total,
                    "oldText must not be empty",
                ));
            }
            let positions: Vec<_> = content.match_indices(&edit.old_text).collect();
            if positions.is_empty() {
                return Err(edit_error(
                    path,
                    index,
                    total,
                    "the old text was not found; it must match exactly including whitespace and newlines",
                ));
            }
            if positions.len() > 1 {
                return Err(edit_error(
                    path,
                    index,
                    total,
                    &format!(
                        "found {} occurrences; oldText must be unique and needs more context",
                        positions.len()
                    ),
                ));
            }
            let start = positions[0].0;
            Ok(MatchedEdit {
                index,
                start,
                end: start + edit.old_text.len(),
                new_text: edit.new_text,
            })
        })
        .collect()
}

fn reject_overlaps(matches: &[MatchedEdit], path: &str) -> Result<(), EditToolError> {
    for pair in matches.windows(2) {
        if pair[0].end > pair[1].start {
            return Err(EditToolError::edit(format!(
                "edits[{}] and edits[{}] overlap in {path}. Merge them into one edit or target disjoint regions.",
                pair[0].index, pair[1].index
            )));
        }
    }
    Ok(())
}

fn edit_error(path: &str, index: usize, total: usize, message: &str) -> EditToolError {
    let target = if total == 1 {
        "replacement".to_owned()
    } else {
        format!("edits[{index}]")
    };
    EditToolError::edit(format!(
        "Could not edit {path}: {target} is invalid because {message}."
    ))
}

fn strip_bom(content: &str) -> (bool, &str) {
    content
        .strip_prefix('\u{feff}')
        .map_or((false, content), |content| (true, content))
}

#[derive(Clone, Copy)]
enum LineEnding {
    Lf,
    Crlf,
}

fn detect_line_ending(content: &str) -> LineEnding {
    content.find('\n').map_or(LineEnding::Lf, |index| {
        if index > 0 && content.as_bytes()[index - 1] == b'\r' {
            LineEnding::Crlf
        } else {
            LineEnding::Lf
        }
    })
}

fn normalize_line_endings(content: &str) -> String {
    content.replace("\r\n", "\n").replace('\r', "\n")
}

fn restore_line_endings(content: &str, ending: LineEnding) -> String {
    match ending {
        LineEnding::Lf => content.to_owned(),
        LineEnding::Crlf => content.replace('\n', "\r\n"),
    }
}

fn line_number(content: &str, byte_offset: usize) -> usize {
    content.as_bytes()[..byte_offset]
        .iter()
        .filter(|value| **value == b'\n')
        .count()
        + 1
}

fn operation_id() -> OperationId {
    OperationId::new(format!("pi-edit-{}", Uuid::now_v7()))
        .expect("UUID operation identifier is valid")
}

fn function_tool(name: &str, description: &str, parameters: Value) -> ToolDefinition {
    let Value::Object(parameters) = parameters else {
        unreachable!("tool parameters are declared as an object")
    };
    ToolDefinition::Function(FunctionTool {
        name: name.to_owned(),
        description: description.to_owned(),
        parameters,
        output_schema: None,
        strict: None,
    })
}

fn parse<T: DeserializeOwned>(arguments: &ToolArguments) -> Result<T, serde_json::Error> {
    match arguments {
        ToolArguments::Object(arguments) => {
            serde_json::from_value(Value::Object(arguments.clone()))
        }
        ToolArguments::String(arguments) => serde_json::from_str(arguments),
    }
}

fn resolve_path(
    descriptor: &MachineDescriptor,
    workspace_root_id: &WorkspaceRootId,
    cwd: &str,
    input: &str,
) -> Result<PathSpec, EditToolError> {
    let root = descriptor
        .workspace_roots
        .iter()
        .find(|root| &root.id == workspace_root_id)
        .ok_or_else(|| {
            EditToolError::invalid_path(format!(
                "workspace root `{workspace_root_id}` is not exposed by the execution runtime"
            ))
        })?;
    let was_absolute = is_absolute_for(descriptor.path_convention, input);
    let input = match descriptor.path_convention {
        PathConvention::Posix if was_absolute => {
            absolute_path_within_root(input, &root.uri, false)?
        }
        PathConvention::Windows if was_absolute => {
            absolute_path_within_root(input, &root.uri, true)?
        }
        PathConvention::Windows => input.replace('\\', "/"),
        PathConvention::Posix => input.to_owned(),
    };
    let base = if was_absolute { "." } else { cwd };
    Ok(PathSpec::workspace(
        workspace_root_id.clone(),
        normalize_relative_path(base, &input)?,
    ))
}

fn normalize_relative_path(base: &str, input: &str) -> Result<String, EditToolError> {
    if input.trim().is_empty() {
        return Err(EditToolError::invalid_path("path must not be empty"));
    }
    if input.contains('\0') {
        return Err(EditToolError::invalid_path(
            "path must not contain a null byte",
        ));
    }
    if input.starts_with('/') || is_windows_absolute(input) {
        return Err(EditToolError::invalid_path(
            "absolute path could not be resolved inside the active workspace root",
        ));
    }

    let mut segments = Vec::new();
    for segment in base.split('/').chain(input.split('/')) {
        match segment {
            "" | "." => {}
            ".." => {
                if segments.pop().is_none() {
                    return Err(EditToolError::invalid_path(
                        "path traverses above the active workspace root",
                    ));
                }
            }
            value => segments.push(value),
        }
    }
    Ok(if segments.is_empty() {
        ".".to_owned()
    } else {
        segments.join("/")
    })
}

fn absolute_path_within_root(
    input: &str,
    root_uri: &str,
    windows: bool,
) -> Result<String, EditToolError> {
    let root_url = Url::parse(root_uri)
        .map_err(|_| EditToolError::invalid_path("workspace root has an invalid absolute URI"))?;
    if root_url.scheme() != "file" {
        return Err(EditToolError::invalid_path(
            "absolute tool paths require a file-backed workspace root",
        ));
    }

    let mut root = if windows {
        root_url.path().to_owned()
    } else {
        root_url
            .to_file_path()
            .map_err(|()| {
                EditToolError::invalid_path(
                    "workspace root file URI cannot be converted to an absolute path",
                )
            })?
            .to_string_lossy()
            .into_owned()
    };
    root.truncate(root.trim_end_matches('/').len());
    let mut input = input.replace('\\', "/");
    if windows {
        root = root.trim_start_matches('/').to_owned();
        input = input.trim_start_matches('/').to_owned();
    }
    let matches_root = if windows {
        input.eq_ignore_ascii_case(&root)
    } else {
        input == root
    };
    if matches_root {
        return Ok(".".to_owned());
    }
    let prefix = format!("{root}/");
    let within_root = if windows {
        input
            .get(..prefix.len())
            .is_some_and(|value| value.eq_ignore_ascii_case(&prefix))
    } else {
        input.starts_with(&prefix)
    };
    if !within_root {
        return Err(EditToolError::invalid_path(
            "absolute path is outside the active workspace root",
        ));
    }
    Ok(input[prefix.len()..].to_owned())
}

fn is_absolute_for(convention: PathConvention, path: &str) -> bool {
    match convention {
        PathConvention::Posix => path.as_bytes().first().is_some_and(|value| *value == b'/'),
        PathConvention::Windows => is_windows_absolute(path),
    }
}

fn is_windows_absolute(path: &str) -> bool {
    let bytes = path.as_bytes();
    (bytes.len() >= 3 && bytes[1] == b':' && (bytes[2] == b'/' || bytes[2] == b'\\'))
        || (bytes.len() >= 2
            && (bytes[0] == b'/' || bytes[0] == b'\\')
            && (bytes[1] == b'/' || bytes[1] == b'\\'))
}
