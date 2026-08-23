use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::{
    BinaryContent, ByteRange, ContentSource, ContinuationCursor, FileRevision, ItemOutcome,
    PathSpec, TimestampMs, Validate, ValidationError,
    validation::{append_nested, finish, issue},
};

/// Kind of entry observed in a target filesystem.
#[derive(Clone, Copy, Debug, Deserialize, Eq, JsonSchema, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum FileKind {
    File,
    Directory,
    Symlink,
    Other,
}

/// Metadata returned by queries and primitive filesystem operations.
#[derive(Clone, Debug, Deserialize, Eq, JsonSchema, PartialEq, Serialize)]
pub struct FileMetadata {
    pub path: PathSpec,
    pub kind: FileKind,
    pub size: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub created_at: Option<TimestampMs>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub modified_at: Option<TimestampMs>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub revision: Option<FileRevision>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mime_type: Option<String>,
}

/// Typed child returned by a directory listing or path search.
#[derive(Clone, Debug, Deserialize, Eq, JsonSchema, PartialEq, Serialize)]
pub struct DirectoryEntry {
    pub path: PathSpec,
    pub name: String,
    pub kind: FileKind,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub size: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub modified_at: Option<TimestampMs>,
}

#[derive(Clone, Debug, Deserialize, Eq, JsonSchema, PartialEq, Serialize)]
pub struct InspectRequest {
    pub path: PathSpec,
    #[serde(default = "default_true")]
    pub follow_symlinks: bool,
}

#[derive(Clone, Debug, Deserialize, Eq, JsonSchema, PartialEq, Serialize)]
pub struct InspectManyRequest {
    pub paths: Vec<PathSpec>,
    #[serde(default = "default_true")]
    pub follow_symlinks: bool,
}

#[derive(Clone, Debug, Deserialize, JsonSchema, PartialEq, Serialize)]
pub struct InspectManyResult {
    pub items: Vec<ItemOutcome<FileMetadata>>,
}

#[derive(Clone, Debug, Deserialize, Eq, JsonSchema, PartialEq, Serialize)]
pub struct ReadBytesRequest {
    pub path: PathSpec,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub range: Option<ByteRange>,
    #[serde(default = "default_true")]
    pub follow_symlinks: bool,
}

#[derive(Clone, Debug, Deserialize, Eq, JsonSchema, PartialEq, Serialize)]
pub struct ReadBytesResult {
    pub metadata: FileMetadata,
    pub content: BinaryContent,
    pub offset: u64,
    pub bytes_returned: u64,
    pub eof: bool,
}

/// Condition checked by a primitive write while the target is locked.
#[derive(Clone, Debug, Deserialize, Eq, JsonSchema, PartialEq, Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum WriteCondition {
    Any,
    MustNotExist,
    MatchRevision { revision: FileRevision },
}

#[derive(Clone, Debug, Deserialize, Eq, JsonSchema, PartialEq, Serialize)]
pub struct WriteBytesRequest {
    pub path: PathSpec,
    pub content: ContentSource,
    pub condition: WriteCondition,
    #[serde(default = "default_true")]
    pub create_parents: bool,
    #[serde(default = "default_true")]
    pub atomic_replace: bool,
    #[serde(default = "default_true")]
    pub follow_symlinks: bool,
}

#[derive(Clone, Debug, Deserialize, Eq, JsonSchema, PartialEq, Serialize)]
pub struct WriteBytesResult {
    pub path: PathSpec,
    pub existed: bool,
    pub revision: FileRevision,
    pub bytes_written: u64,
}

#[derive(Clone, Debug, Deserialize, Eq, JsonSchema, PartialEq, Serialize)]
pub struct CreateDirectoryRequest {
    pub path: PathSpec,
    #[serde(default = "default_true")]
    pub recursive: bool,
}

#[derive(Clone, Debug, Deserialize, Eq, JsonSchema, PartialEq, Serialize)]
pub struct RemovePathRequest {
    pub path: PathSpec,
    #[serde(default)]
    pub recursive: bool,
    #[serde(default)]
    pub force: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub expected_revision: Option<FileRevision>,
}

#[derive(Clone, Debug, Deserialize, Eq, JsonSchema, PartialEq, Serialize)]
pub struct MovePathRequest {
    pub source: PathSpec,
    pub destination: PathSpec,
    #[serde(default)]
    pub overwrite: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub expected_source_revision: Option<FileRevision>,
}

#[derive(Clone, Debug, Deserialize, Eq, JsonSchema, PartialEq, Serialize)]
pub struct CopyPathRequest {
    pub source: PathSpec,
    pub destination: PathSpec,
    #[serde(default)]
    pub recursive: bool,
    #[serde(default)]
    pub overwrite: bool,
}

#[derive(Clone, Debug, Deserialize, Eq, JsonSchema, PartialEq, Serialize)]
pub struct WalkRequest {
    pub root: PathSpec,
    pub max_depth: u32,
    pub max_entries: u32,
    #[serde(default)]
    pub follow_directory_symlinks: bool,
    #[serde(default)]
    pub include_hidden: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cursor: Option<ContinuationCursor>,
}

#[derive(Clone, Debug, Deserialize, Eq, JsonSchema, PartialEq, Serialize)]
pub struct WalkResult {
    pub entries: Vec<DirectoryEntry>,
    pub truncated: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cursor: Option<ContinuationCursor>,
}

impl Validate for InspectManyRequest {
    fn validate(&self) -> Result<(), ValidationError> {
        let mut issues = Vec::new();
        if self.paths.is_empty() {
            issue(&mut issues, "inspect_many.paths", "must not be empty");
        }
        for (index, path) in self.paths.iter().enumerate() {
            append_nested(
                &mut issues,
                format_args!("inspect_many.paths[{index}]"),
                path.validate(),
            );
        }
        finish(issues)
    }
}

impl Validate for ReadBytesRequest {
    fn validate(&self) -> Result<(), ValidationError> {
        let mut issues = Vec::new();
        append_nested(&mut issues, "read_bytes.path", self.path.validate());
        if self.range.is_some_and(|range| range.length == 0) {
            issue(
                &mut issues,
                "read_bytes.range.length",
                "must be greater than zero",
            );
        }
        finish(issues)
    }
}

impl Validate for WalkRequest {
    fn validate(&self) -> Result<(), ValidationError> {
        let mut issues = Vec::new();
        append_nested(&mut issues, "walk.root", self.root.validate());
        if self.max_entries == 0 {
            issue(&mut issues, "walk.max_entries", "must be greater than zero");
        }
        finish(issues)
    }
}

const fn default_true() -> bool {
    true
}
