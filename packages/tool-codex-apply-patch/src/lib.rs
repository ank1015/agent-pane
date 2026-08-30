//! Reusable implementation of Codex's `apply_patch` custom tool.
//!
//! The crate owns the exact model-facing grammar, patch parsing, Codex-style
//! context matching, complete-file preflight, and revision-fenced mutation.
//! Harnesses remain responsible for selecting the tool and recording its
//! output in the transcript.

mod file_update;
mod parser;

use execution_contracts::{
    ApplyMutationRequest, ContentSource, ContinuationCursor, FileRevision, MachineDescriptor,
    MutationAtomicity, MutationOperation, MutationPlan, MutationPostActions, OperationId,
    PathConvention, PathSpec, ReadMode, ReadRequest, ReadResult, TextPageRequest, WorkspaceRootId,
};
use execution_runtime::{ExecutionRuntime, OperationContext};
use llm_contracts::{
    ContentPart, CustomTool, CustomToolFormat, GrammarSyntax, TextContent, ToolArguments,
    ToolDefinition,
};
use serde_json::{Value, json};
use url::Url;
use uuid::Uuid;

pub use parser::{Hunk, ParseError, UpdateFileChunk, parse_patch};

/// Name used in the model-facing custom tool definition.
pub const TOOL_NAME: &str = "apply_patch";
/// Codex's model-facing Lark grammar for apply-patch calls.
pub const APPLY_PATCH_LARK_GRAMMAR: &str = include_str!("apply_patch.lark");
/// Maximum lines requested per page while preflighting an update.
pub const PATCH_PAGE_LINES: u32 = 100_000;
/// Maximum bytes requested per page and per line while preflighting an update.
pub const PATCH_PAGE_BYTES: u64 = 8 * 1_024 * 1_024;

/// Execution inputs specific to the apply-patch tool.
pub struct ApplyPatchToolContext<'a> {
    runtime: &'a dyn ExecutionRuntime,
    operation: &'a OperationContext,
    workspace_root_id: WorkspaceRootId,
    cwd: String,
}

impl<'a> ApplyPatchToolContext<'a> {
    /// Creates an apply-patch context rooted at `workspace_root_id` and `cwd`.
    pub fn new(
        runtime: &'a dyn ExecutionRuntime,
        operation: &'a OperationContext,
        workspace_root_id: WorkspaceRootId,
        cwd: impl Into<String>,
    ) -> Result<Self, ApplyPatchToolError> {
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

    fn resolve_path(&self, input: &str) -> Result<PathSpec, ApplyPatchToolError> {
        resolve_path(
            self.runtime.descriptor(),
            &self.workspace_root_id,
            &self.cwd,
            input,
        )
    }
}

/// Successful output from the apply-patch tool.
#[derive(Clone, Debug, PartialEq)]
pub struct ApplyPatchToolOutput {
    pub content: Vec<ContentPart>,
    pub details: Option<Value>,
}

impl ApplyPatchToolOutput {
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

/// Structured failure that a harness can map into its own tool result.
#[derive(Debug, thiserror::Error)]
#[error("{message}")]
pub struct ApplyPatchToolError {
    name: &'static str,
    message: String,
    details: Option<Value>,
}

impl ApplyPatchToolError {
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

    fn verification(message: impl std::fmt::Display) -> Self {
        Self::new(
            "apply_patch_failed",
            format!("apply_patch verification failed: {message}"),
        )
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

impl From<execution_contracts::ExecutionError> for ApplyPatchToolError {
    fn from(error: execution_contracts::ExecutionError) -> Self {
        let details = serde_json::to_value(&error).ok();
        Self {
            name: "execution_error",
            message: error.message,
            details,
        }
    }
}

/// Returns Codex's model-facing freeform custom-tool definition.
#[must_use]
pub fn definition() -> ToolDefinition {
    ToolDefinition::Custom(CustomTool {
        name: TOOL_NAME.to_owned(),
        description: "The `apply_patch` tool can be used to edit files. This is a FREEFORM tool, so do not wrap the patch in JSON.".to_owned(),
        format: CustomToolFormat {
            syntax: GrammarSyntax::Lark,
            definition: APPLY_PATCH_LARK_GRAMMAR.to_owned(),
        },
    })
}

/// Extracts the raw patch text supplied to the custom tool.
pub fn parse_arguments(arguments: &ToolArguments) -> Result<&str, ApplyPatchToolError> {
    match arguments {
        ToolArguments::String(patch) => Ok(patch),
        ToolArguments::Object(_) => Err(ApplyPatchToolError::invalid_arguments(
            "expected raw patch text, not a JSON object",
        )),
    }
}

/// Parses and executes a provider-neutral custom tool call.
pub async fn execute_apply_patch_tool(
    arguments: &ToolArguments,
    context: &ApplyPatchToolContext<'_>,
) -> Result<ApplyPatchToolOutput, ApplyPatchToolError> {
    execute(parse_arguments(arguments)?, context).await
}

/// Parses, verifies, and applies one Codex-format patch.
pub async fn execute(
    patch: &str,
    context: &ApplyPatchToolContext<'_>,
) -> Result<ApplyPatchToolOutput, ApplyPatchToolError> {
    let hunks = parse_patch(patch).map_err(ApplyPatchToolError::verification)?;
    if hunks.is_empty() {
        return Err(ApplyPatchToolError::verification("No files were modified."));
    }

    let mut operations = Vec::with_capacity(hunks.len() + 1);
    let mut source_targets = Vec::with_capacity(hunks.len());
    let mut added = Vec::new();
    let mut modified = Vec::new();
    let mut deleted = Vec::new();

    for hunk in hunks {
        let source_label = hunk.source_path().to_string_lossy().into_owned();
        let source = context.resolve_path(&source_label)?;
        if source_targets.contains(&source) {
            return Err(ApplyPatchToolError::verification(format!(
                "invalid patch: multiple operations target {source_label}"
            )));
        }
        source_targets.push(source.clone());

        match hunk {
            Hunk::AddFile { path, contents } => {
                added.push(path.to_string_lossy().into_owned());
                operations.push(MutationOperation::PutFile {
                    path: source,
                    content: ContentSource::Text { content: contents },
                    create_parents: true,
                    preserve_utf8_bom: false,
                    expected_revision: None,
                });
            }
            Hunk::DeleteFile { path } => {
                let path_label = path.to_string_lossy().into_owned();
                let (_, revision) =
                    read_complete_text(context, source.clone())
                        .await
                        .map_err(|error| {
                            ApplyPatchToolError::verification(format!(
                                "Failed to read {path_label}: {}",
                                error.message()
                            ))
                        })?;
                deleted.push(path_label);
                operations.push(MutationOperation::Remove {
                    path: source,
                    recursive: false,
                    force: false,
                    expected_revision: revision,
                });
            }
            Hunk::UpdateFile {
                path,
                move_path,
                chunks,
            } => {
                let path_label = path.to_string_lossy().into_owned();
                let (original, revision) = read_complete_text(context, source.clone())
                    .await
                    .map_err(|error| {
                        ApplyPatchToolError::verification(format!(
                            "Failed to read file to update {path_label}: {}",
                            error.message()
                        ))
                    })?;
                let new_content = file_update::derive_new_contents(&original, &path_label, &chunks)
                    .map_err(ApplyPatchToolError::verification)?;

                if let Some(destination_path) = move_path {
                    let destination_label = destination_path.to_string_lossy().into_owned();
                    let destination = context.resolve_path(&destination_label)?;
                    if destination == source {
                        return Err(ApplyPatchToolError::verification(format!(
                            "move destination is the same as source: {path_label}"
                        )));
                    }
                    modified.push(destination_label);
                    operations.push(MutationOperation::PutFile {
                        path: destination,
                        content: ContentSource::Text {
                            content: new_content,
                        },
                        create_parents: true,
                        preserve_utf8_bom: false,
                        expected_revision: None,
                    });
                    operations.push(MutationOperation::Remove {
                        path: source,
                        recursive: false,
                        force: false,
                        expected_revision: revision,
                    });
                } else {
                    modified.push(path_label);
                    operations.push(MutationOperation::PutFile {
                        path: source,
                        content: ContentSource::Text {
                            content: new_content,
                        },
                        create_parents: false,
                        preserve_utf8_bom: false,
                        expected_revision: revision,
                    });
                }
            }
        }
    }

    let mutation = context
        .runtime
        .workspace_mutation()
        .ok_or_else(|| ApplyPatchToolError::missing_capability("workspace mutations"))?;
    let result = mutation
        .apply(
            context.operation,
            ApplyMutationRequest {
                operation_id: operation_id(),
                plan: MutationPlan {
                    operations,
                    atomicity: MutationAtomicity::AtomicIfSupported,
                    post_actions: MutationPostActions::default(),
                },
            },
        )
        .await?;

    let summary = success_summary(&added, &modified, &deleted);
    Ok(ApplyPatchToolOutput::text(summary).with_details(json!({
        "added": added,
        "modified": modified,
        "deleted": deleted,
        "mutation": result
    })))
}

fn success_summary(added: &[String], modified: &[String], deleted: &[String]) -> String {
    let mut output = "Success. Updated the following files:\n".to_owned();
    for path in added {
        output.push_str("A ");
        output.push_str(path);
        output.push('\n');
    }
    for path in modified {
        output.push_str("M ");
        output.push_str(path);
        output.push('\n');
    }
    for path in deleted {
        output.push_str("D ");
        output.push_str(path);
        output.push('\n');
    }
    output
}

async fn read_complete_text(
    context: &ApplyPatchToolContext<'_>,
    path: PathSpec,
) -> Result<(String, Option<FileRevision>), ApplyPatchToolError> {
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
                        max_lines: PATCH_PAGE_LINES,
                        max_bytes: PATCH_PAGE_BYTES,
                        max_line_bytes: PATCH_PAGE_BYTES,
                        include_total_lines: false,
                    }),
                },
            )
            .await?;
        let ReadResult::Text { page } = result else {
            return Err(ApplyPatchToolError::process(
                "apply_patch requires a UTF-8 text file",
            ));
        };
        if page.lines_truncated {
            return Err(ApplyPatchToolError::process(format!(
                "file contains a line larger than the {PATCH_PAGE_BYTES} byte patch page limit"
            )));
        }
        if let Some(expected) = &revision {
            if page.metadata.revision.as_ref() != Some(expected) {
                return Err(ApplyPatchToolError::process(
                    "file changed while it was being read",
                ));
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
            return Err(ApplyPatchToolError::process(
                "text read was truncated without a continuation",
            ));
        }
    }
    Ok((content, revision))
}

fn operation_id() -> OperationId {
    OperationId::new(format!("codex-apply-patch-{}", Uuid::now_v7()))
        .expect("UUID operation identifier is valid")
}

fn resolve_path(
    descriptor: &MachineDescriptor,
    workspace_root_id: &WorkspaceRootId,
    cwd: &str,
    input: &str,
) -> Result<PathSpec, ApplyPatchToolError> {
    let root = descriptor
        .workspace_roots
        .iter()
        .find(|root| &root.id == workspace_root_id)
        .ok_or_else(|| {
            ApplyPatchToolError::invalid_path(format!(
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

fn normalize_relative_path(base: &str, input: &str) -> Result<String, ApplyPatchToolError> {
    if input.trim().is_empty() {
        return Err(ApplyPatchToolError::invalid_path("path must not be empty"));
    }
    if input.contains('\0') {
        return Err(ApplyPatchToolError::invalid_path(
            "path must not contain a null byte",
        ));
    }
    if input.starts_with('/') || is_windows_absolute(input) {
        return Err(ApplyPatchToolError::invalid_path(
            "absolute path could not be resolved inside the active workspace root",
        ));
    }

    let mut segments = Vec::new();
    for segment in base.split('/').chain(input.split('/')) {
        match segment {
            "" | "." => {}
            ".." => {
                if segments.pop().is_none() {
                    return Err(ApplyPatchToolError::invalid_path(
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
) -> Result<String, ApplyPatchToolError> {
    let root_url = Url::parse(root_uri).map_err(|_| {
        ApplyPatchToolError::invalid_path("workspace root has an invalid absolute URI")
    })?;
    if root_url.scheme() != "file" {
        return Err(ApplyPatchToolError::invalid_path(
            "absolute tool paths require a file-backed workspace root",
        ));
    }
    let mut root = if windows {
        root_url.path().to_owned()
    } else {
        root_url
            .to_file_path()
            .map_err(|()| {
                ApplyPatchToolError::invalid_path(
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
        return Err(ApplyPatchToolError::invalid_path(
            "absolute path is outside the active workspace root",
        ));
    }
    Ok(input[prefix.len()..].to_owned())
}

fn is_absolute_for(convention: PathConvention, path: &str) -> bool {
    match convention {
        PathConvention::Posix => path.starts_with('/'),
        PathConvention::Windows => is_windows_absolute(path),
    }
}

fn is_windows_absolute(path: &str) -> bool {
    let bytes = path.as_bytes();
    (bytes.len() >= 3 && bytes[1] == b':' && matches!(bytes[2], b'/' | b'\\'))
        || (bytes.len() >= 2
            && matches!(bytes[0], b'/' | b'\\')
            && matches!(bytes[1], b'/' | b'\\'))
}
