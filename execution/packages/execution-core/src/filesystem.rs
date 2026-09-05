use async_trait::async_trait;
use serde::{Deserialize, Serialize};

use crate::{
    BinaryData, DirectoryCursor, ExecutionResult, FileRevision, OperationContext, OperationId,
    RootId, TimestampMs, Validate, ValidationError,
    validation::{append_nested, finish, issue, require_non_empty},
};

/// Portable path relative to a configured execution root.
#[derive(Clone, Debug, Deserialize, Eq, Hash, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ExecutionPath {
    pub root_id: RootId,
    /// Slash-separated path. `.` addresses the root itself.
    pub path: String,
}

impl ExecutionPath {
    pub fn new(root_id: RootId, path: impl Into<String>) -> Result<Self, ValidationError> {
        let value = Self {
            root_id,
            path: path.into(),
        };
        value.validate()?;
        Ok(value)
    }

    #[must_use]
    pub fn root(root_id: RootId) -> Self {
        Self {
            root_id,
            path: ".".to_string(),
        }
    }
}

impl Validate for ExecutionPath {
    fn validate(&self) -> Result<(), ValidationError> {
        let mut issues = Vec::new();
        require_non_empty(&mut issues, "path", &self.path);

        if self.path.contains('\0') {
            issue(&mut issues, "path", "must not contain a null byte");
        }
        if self.path.starts_with('/') || self.path.starts_with('\\') {
            issue(&mut issues, "path", "must be relative to its root");
        }
        if self.path.contains('\\') {
            issue(&mut issues, "path", "must use forward slashes");
        }
        if self.path.len() >= 2 && self.path.as_bytes()[1] == b':' {
            issue(&mut issues, "path", "must not contain a drive prefix");
        }

        if self.path != "." {
            if self.path.ends_with('/') {
                issue(&mut issues, "path", "must not end with a slash");
            }
            for segment in self.path.split('/') {
                if segment.is_empty() {
                    issue(&mut issues, "path", "must not contain empty segments");
                } else if segment == "." {
                    issue(&mut issues, "path", "must not contain dot segments");
                } else if segment == ".." {
                    issue(&mut issues, "path", "must not traverse above its root");
                }
            }
        }

        finish(issues)
    }
}

/// Kind of filesystem entry observed on an execution host.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum FileKind {
    File,
    Directory,
    Symlink,
    Other,
}

/// Metadata returned by filesystem operations.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct FileMetadata {
    pub path: ExecutionPath,
    pub kind: FileKind,
    pub size: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub modified_at: Option<TimestampMs>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub revision: Option<FileRevision>,
}

/// Typed entry in a directory listing.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct DirectoryEntry {
    pub name: String,
    pub metadata: FileMetadata,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct StatRequest {
    pub path: ExecutionPath,
    #[serde(default = "default_true")]
    pub follow_symlinks: bool,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ReadFileRequest {
    pub path: ExecutionPath,
    #[serde(default)]
    pub offset: u64,
    pub max_bytes: u64,
    #[serde(default = "default_true")]
    pub follow_symlinks: bool,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ReadFileResult {
    pub metadata: FileMetadata,
    pub data: BinaryData,
    pub offset: u64,
    pub eof: bool,
}

/// Condition checked while the destination is locked for replacement.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum WriteCondition {
    Any,
    MustNotExist,
    MatchRevision { revision: FileRevision },
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct WriteFileRequest {
    pub operation_id: OperationId,
    pub path: ExecutionPath,
    pub data: BinaryData,
    pub condition: WriteCondition,
    #[serde(default = "default_true")]
    pub create_parents: bool,
    #[serde(default = "default_true")]
    pub follow_symlinks: bool,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct WriteFileResult {
    pub path: ExecutionPath,
    pub existed: bool,
    pub revision: FileRevision,
    pub bytes_written: u64,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct CreateDirectoryRequest {
    pub operation_id: OperationId,
    pub path: ExecutionPath,
    #[serde(default = "default_true")]
    pub recursive: bool,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct RemovePathRequest {
    pub operation_id: OperationId,
    pub path: ExecutionPath,
    #[serde(default)]
    pub recursive: bool,
    #[serde(default)]
    pub ignore_missing: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub expected_revision: Option<FileRevision>,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct RemovePathResult {
    pub removed: bool,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ListDirectoryRequest {
    pub path: ExecutionPath,
    #[serde(default = "default_true")]
    pub follow_symlinks: bool,
    pub max_entries: u32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cursor: Option<DirectoryCursor>,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ListDirectoryResult {
    pub entries: Vec<DirectoryEntry>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub next_cursor: Option<DirectoryCursor>,
}

impl Validate for StatRequest {
    fn validate(&self) -> Result<(), ValidationError> {
        self.path.validate()
    }
}

impl Validate for ReadFileRequest {
    fn validate(&self) -> Result<(), ValidationError> {
        let mut issues = Vec::new();
        append_nested(&mut issues, "path", self.path.validate());
        if self.max_bytes == 0 {
            issue(&mut issues, "max_bytes", "must be greater than zero");
        }
        finish(issues)
    }
}

impl Validate for WriteFileRequest {
    fn validate(&self) -> Result<(), ValidationError> {
        let mut issues = Vec::new();
        append_nested(&mut issues, "path", self.path.validate());
        finish(issues)
    }
}

impl Validate for CreateDirectoryRequest {
    fn validate(&self) -> Result<(), ValidationError> {
        self.path.validate()
    }
}

impl Validate for RemovePathRequest {
    fn validate(&self) -> Result<(), ValidationError> {
        self.path.validate()
    }
}

impl Validate for ListDirectoryRequest {
    fn validate(&self) -> Result<(), ValidationError> {
        let mut issues = Vec::new();
        append_nested(&mut issues, "path", self.path.validate());
        if self.max_entries == 0 {
            issue(&mut issues, "max_entries", "must be greater than zero");
        }
        finish(issues)
    }
}

#[async_trait]
pub trait FileSystem: Send + Sync {
    async fn stat(
        &self,
        context: &OperationContext,
        request: StatRequest,
    ) -> ExecutionResult<FileMetadata>;

    async fn read(
        &self,
        context: &OperationContext,
        request: ReadFileRequest,
    ) -> ExecutionResult<ReadFileResult>;

    async fn write(
        &self,
        context: &OperationContext,
        request: WriteFileRequest,
    ) -> ExecutionResult<WriteFileResult>;

    async fn create_directory(
        &self,
        context: &OperationContext,
        request: CreateDirectoryRequest,
    ) -> ExecutionResult<()>;

    async fn remove(
        &self,
        context: &OperationContext,
        request: RemovePathRequest,
    ) -> ExecutionResult<RemovePathResult>;

    async fn list(
        &self,
        context: &OperationContext,
        request: ListDirectoryRequest,
    ) -> ExecutionResult<ListDirectoryResult>;
}

const fn default_true() -> bool {
    true
}

#[cfg(test)]
mod tests {
    use super::*;

    fn root_id() -> RootId {
        RootId::new("workspace").expect("valid root ID")
    }

    #[test]
    fn execution_path_accepts_root_and_portable_relative_paths() {
        ExecutionPath::new(root_id(), ".").expect("root path should be valid");
        ExecutionPath::new(root_id(), "src/main.rs").expect("relative path should be valid");
    }

    #[test]
    fn execution_path_rejects_escape_and_non_canonical_paths() {
        for path in ["../secret", "/etc/passwd", "src\\main.rs", "src//main.rs"] {
            ExecutionPath::new(root_id(), path).expect_err("invalid path should fail");
        }
    }

    #[test]
    fn bounded_reads_require_a_positive_limit() {
        let request = ReadFileRequest {
            path: ExecutionPath::root(root_id()),
            offset: 0,
            max_bytes: 0,
            follow_symlinks: true,
        };
        let error = request.validate().expect_err("zero limit should fail");
        assert!(error.issues.iter().any(|value| value.path == "max_bytes"));
    }

    #[test]
    fn bounded_listings_require_a_positive_limit() {
        let request = ListDirectoryRequest {
            path: ExecutionPath::root(root_id()),
            follow_symlinks: true,
            max_entries: 0,
            cursor: None,
        };
        request.validate().expect_err("zero limit should fail");
    }
}
