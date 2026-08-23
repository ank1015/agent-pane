use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::{
    ContentSource, FileRevision, OperationId, PathSpec, PreparedMutationId, TimestampMs, Validate,
    ValidationError,
    validation::{append_nested, finish, issue},
};

#[derive(Clone, Copy, Debug, Deserialize, Eq, JsonSchema, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum OccurrencePolicy {
    Unique,
    First,
    All,
}

#[derive(Clone, Debug, Deserialize, Eq, JsonSchema, PartialEq, Serialize)]
pub struct TextReplacement {
    pub old_text: String,
    pub new_text: String,
    pub occurrence: OccurrencePolicy,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, JsonSchema, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum PatchMatchPolicy {
    Exact,
    Context,
    WhitespaceTolerant,
}

/// Canonical line-based patch hunk parsed from a model-facing patch dialect.
#[derive(Clone, Debug, Deserialize, Eq, JsonSchema, PartialEq, Serialize)]
pub struct TextPatchHunk {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub context_hint: Option<String>,
    #[serde(default)]
    pub old_lines: Vec<String>,
    #[serde(default)]
    pub new_lines: Vec<String>,
    #[serde(default)]
    pub end_of_file: bool,
}

#[derive(Clone, Debug, Deserialize, Eq, JsonSchema, PartialEq, Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum MutationOperation {
    PutFile {
        path: PathSpec,
        content: ContentSource,
        #[serde(default = "default_true")]
        create_parents: bool,
        #[serde(default)]
        preserve_utf8_bom: bool,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        expected_revision: Option<FileRevision>,
    },
    CreateFile {
        path: PathSpec,
        content: ContentSource,
        #[serde(default = "default_true")]
        create_parents: bool,
    },
    ReplaceText {
        path: PathSpec,
        replacements: Vec<TextReplacement>,
        #[serde(default = "default_true")]
        preserve_line_endings: bool,
        #[serde(default = "default_true")]
        preserve_utf8_bom: bool,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        expected_revision: Option<FileRevision>,
    },
    ApplyTextPatch {
        path: PathSpec,
        hunks: Vec<TextPatchHunk>,
        match_policy: PatchMatchPolicy,
        #[serde(default = "default_true")]
        preserve_line_endings: bool,
        #[serde(default = "default_true")]
        preserve_utf8_bom: bool,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        expected_revision: Option<FileRevision>,
    },
    Remove {
        path: PathSpec,
        #[serde(default)]
        recursive: bool,
        #[serde(default)]
        force: bool,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        expected_revision: Option<FileRevision>,
    },
    Move {
        source: PathSpec,
        destination: PathSpec,
        #[serde(default)]
        overwrite: bool,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        expected_source_revision: Option<FileRevision>,
    },
    Copy {
        source: PathSpec,
        destination: PathSpec,
        #[serde(default)]
        recursive: bool,
        #[serde(default)]
        overwrite: bool,
    },
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, JsonSchema, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum MutationAtomicity {
    BestEffort,
    ValidateAll,
    AtomicIfSupported,
}

#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, JsonSchema, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum FormatMode {
    #[default]
    None,
    Configured,
}

#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, JsonSchema, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum DiagnosticsMode {
    #[default]
    None,
    ChangedFiles,
    Workspace,
}

#[derive(Clone, Debug, Default, Deserialize, Eq, JsonSchema, PartialEq, Serialize)]
pub struct MutationPostActions {
    #[serde(default)]
    pub format: FormatMode,
    #[serde(default)]
    pub diagnostics: DiagnosticsMode,
}

#[derive(Clone, Debug, Deserialize, Eq, JsonSchema, PartialEq, Serialize)]
pub struct MutationPlan {
    pub operations: Vec<MutationOperation>,
    pub atomicity: MutationAtomicity,
    #[serde(default)]
    pub post_actions: MutationPostActions,
}

#[derive(Clone, Debug, Deserialize, Eq, JsonSchema, PartialEq, Serialize)]
pub struct PrepareMutationRequest {
    pub plan: MutationPlan,
}

#[derive(Clone, Debug, Deserialize, Eq, JsonSchema, PartialEq, Serialize)]
pub struct MutationPreview {
    pub operation_index: u32,
    pub path: PathSpec,
    pub change: FileChangeKind,
    pub diff: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub base_revision: Option<FileRevision>,
}

#[derive(Clone, Debug, Deserialize, Eq, JsonSchema, PartialEq, Serialize)]
pub struct PreparedMutation {
    pub prepared_id: PreparedMutationId,
    pub expires_at: TimestampMs,
    pub previews: Vec<MutationPreview>,
    pub atomic_commit_supported: bool,
}

#[derive(Clone, Debug, Deserialize, Eq, JsonSchema, PartialEq, Serialize)]
pub struct CommitMutationRequest {
    pub prepared_id: PreparedMutationId,
    pub operation_id: OperationId,
}

#[derive(Clone, Debug, Deserialize, Eq, JsonSchema, PartialEq, Serialize)]
pub struct AbortMutationRequest {
    pub prepared_id: PreparedMutationId,
}

#[derive(Clone, Debug, Deserialize, Eq, JsonSchema, PartialEq, Serialize)]
pub struct ApplyMutationRequest {
    pub operation_id: OperationId,
    pub plan: MutationPlan,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, JsonSchema, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum FileChangeKind {
    Created,
    Modified,
    Removed,
    Moved,
    Copied,
}

#[derive(Clone, Debug, Deserialize, Eq, JsonSchema, PartialEq, Serialize)]
pub struct AppliedChange {
    pub operation_index: u32,
    pub path: PathSpec,
    pub change: FileChangeKind,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub revision: Option<FileRevision>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub diff: Option<String>,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, JsonSchema, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum MutationStatus {
    Committed,
    PartiallyCommitted,
    RolledBack,
}

#[derive(Clone, Debug, Deserialize, JsonSchema, PartialEq, Serialize)]
pub struct MutationResult {
    pub operation_id: OperationId,
    pub status: MutationStatus,
    pub changes: Vec<AppliedChange>,
    pub atomic: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub diagnostics: Option<serde_json::Value>,
}

impl Validate for MutationPlan {
    fn validate(&self) -> Result<(), ValidationError> {
        let mut issues = Vec::new();
        if self.operations.is_empty() {
            issue(&mut issues, "mutation.operations", "must not be empty");
        }
        for (index, operation) in self.operations.iter().enumerate() {
            validate_operation(operation, index, &mut issues);
        }
        finish(issues)
    }
}

impl Validate for PrepareMutationRequest {
    fn validate(&self) -> Result<(), ValidationError> {
        self.plan.validate()
    }
}

impl Validate for ApplyMutationRequest {
    fn validate(&self) -> Result<(), ValidationError> {
        self.plan.validate()
    }
}

fn validate_operation(
    operation: &MutationOperation,
    index: usize,
    issues: &mut Vec<crate::ValidationIssue>,
) {
    let prefix = format!("mutation.operations[{index}]");
    match operation {
        MutationOperation::PutFile { path, .. }
        | MutationOperation::CreateFile { path, .. }
        | MutationOperation::Remove { path, .. } => {
            append_nested(issues, format_args!("{prefix}.path"), path.validate());
        }
        MutationOperation::ReplaceText {
            path, replacements, ..
        } => {
            append_nested(issues, format_args!("{prefix}.path"), path.validate());
            if replacements.is_empty() {
                issue(
                    issues,
                    format!("{prefix}.replacements"),
                    "must not be empty",
                );
            }
            for (replacement_index, replacement) in replacements.iter().enumerate() {
                if replacement.old_text.is_empty() {
                    issue(
                        issues,
                        format!("{prefix}.replacements[{replacement_index}].old_text"),
                        "must not be empty",
                    );
                }
                if replacement.old_text == replacement.new_text {
                    issue(
                        issues,
                        format!("{prefix}.replacements[{replacement_index}]"),
                        "old_text and new_text must differ",
                    );
                }
            }
        }
        MutationOperation::ApplyTextPatch { path, hunks, .. } => {
            append_nested(issues, format_args!("{prefix}.path"), path.validate());
            if hunks.is_empty() {
                issue(issues, format!("{prefix}.hunks"), "must not be empty");
            }
            for (hunk_index, hunk) in hunks.iter().enumerate() {
                if hunk.old_lines.is_empty() && hunk.new_lines.is_empty() {
                    issue(
                        issues,
                        format!("{prefix}.hunks[{hunk_index}]"),
                        "must change at least one line",
                    );
                }
            }
        }
        MutationOperation::Move {
            source,
            destination,
            ..
        }
        | MutationOperation::Copy {
            source,
            destination,
            ..
        } => {
            append_nested(issues, format_args!("{prefix}.source"), source.validate());
            append_nested(
                issues,
                format_args!("{prefix}.destination"),
                destination.validate(),
            );
            if source == destination {
                issue(issues, prefix, "source and destination must be different");
            }
        }
    }
}

const fn default_true() -> bool {
    true
}
