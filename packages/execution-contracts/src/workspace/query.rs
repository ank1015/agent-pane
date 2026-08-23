use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::{
    ArtifactId, ContinuationCursor, DirectoryEntry, FileMetadata, FileRevision, PathSpec, Validate,
    ValidationError,
    validation::{append_nested, finish, issue},
};

pub const DEFAULT_READ_MAX_LINES: u32 = 2_000;
pub const DEFAULT_READ_MAX_BYTES: u64 = 50 * 1_024;
pub const DEFAULT_READ_MAX_LINE_BYTES: u64 = 8 * 1_024;

#[derive(Clone, Copy, Debug, Deserialize, Eq, JsonSchema, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ReadMode {
    Auto,
    Text,
    Bytes,
    Media,
}

#[derive(Clone, Debug, Deserialize, Eq, JsonSchema, PartialEq, Serialize)]
pub struct TextPageRequest {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub start_line: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cursor: Option<ContinuationCursor>,
    pub max_lines: u32,
    pub max_bytes: u64,
    pub max_line_bytes: u64,
    #[serde(default)]
    pub include_total_lines: bool,
}

impl Default for TextPageRequest {
    fn default() -> Self {
        Self {
            start_line: None,
            cursor: None,
            max_lines: DEFAULT_READ_MAX_LINES,
            max_bytes: DEFAULT_READ_MAX_BYTES,
            max_line_bytes: DEFAULT_READ_MAX_LINE_BYTES,
            include_total_lines: false,
        }
    }
}

#[derive(Clone, Debug, Deserialize, Eq, JsonSchema, PartialEq, Serialize)]
pub struct ReadRequest {
    pub path: PathSpec,
    pub mode: ReadMode,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub page: Option<TextPageRequest>,
}

#[derive(Clone, Debug, Deserialize, Eq, JsonSchema, PartialEq, Serialize)]
pub struct TextPage {
    pub metadata: FileMetadata,
    pub content: String,
    pub start_line: u64,
    pub end_line: u64,
    pub has_more: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub next_line: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cursor: Option<ContinuationCursor>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub total_lines: Option<u64>,
    #[serde(default)]
    pub lines_truncated: bool,
}

#[derive(Clone, Debug, Deserialize, Eq, JsonSchema, PartialEq, Serialize)]
pub struct ArtifactRead {
    pub metadata: FileMetadata,
    pub artifact_id: ArtifactId,
    pub mime_type: String,
}

#[derive(Clone, Debug, Deserialize, Eq, JsonSchema, PartialEq, Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ReadResult {
    Text { page: TextPage },
    Media { artifact: ArtifactRead },
    Binary { artifact: ArtifactRead },
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, JsonSchema, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ListSort {
    NameAscending,
    NameDescending,
    ModifiedAscending,
    ModifiedDescending,
}

#[derive(Clone, Debug, Deserialize, Eq, JsonSchema, PartialEq, Serialize)]
pub struct ListRequest {
    pub path: PathSpec,
    pub limit: u32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cursor: Option<ContinuationCursor>,
    pub sort: ListSort,
    #[serde(default = "default_true")]
    pub include_hidden: bool,
    #[serde(default)]
    pub follow_symlinks: bool,
}

#[derive(Clone, Debug, Deserialize, Eq, JsonSchema, PartialEq, Serialize)]
pub struct ListResult {
    pub directory: FileMetadata,
    pub entries: Vec<DirectoryEntry>,
    pub truncated: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cursor: Option<ContinuationCursor>,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, JsonSchema, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum SearchKind {
    Paths,
    Content,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, JsonSchema, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum SearchSyntax {
    Glob,
    Regex,
    Literal,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, JsonSchema, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum IgnoreMode {
    Git,
    Standard,
    None,
}

#[derive(Clone, Debug, Deserialize, Eq, JsonSchema, PartialEq, Serialize)]
pub struct SearchRequest {
    pub root: PathSpec,
    pub kind: SearchKind,
    pub pattern: String,
    pub syntax: SearchSyntax,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub include: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub exclude: Vec<String>,
    pub ignore_mode: IgnoreMode,
    #[serde(default)]
    pub hidden: bool,
    #[serde(default)]
    pub follow_symlinks: bool,
    #[serde(default)]
    pub context_before: u32,
    #[serde(default)]
    pub context_after: u32,
    pub max_results: u32,
    pub max_bytes: u64,
    pub max_line_bytes: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cursor: Option<ContinuationCursor>,
}

#[derive(Clone, Debug, Deserialize, Eq, JsonSchema, PartialEq, Serialize)]
pub struct TextSubmatch {
    pub start: u64,
    pub end: u64,
    pub text: String,
}

#[derive(Clone, Debug, Deserialize, Eq, JsonSchema, PartialEq, Serialize)]
pub struct TextMatch {
    pub path: PathSpec,
    pub line: u64,
    pub byte_offset: u64,
    pub text: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub before: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub after: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub submatches: Vec<TextSubmatch>,
    #[serde(default)]
    pub line_truncated: bool,
}

#[derive(Clone, Debug, Deserialize, Eq, JsonSchema, PartialEq, Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum SearchItem {
    Path { entry: DirectoryEntry },
    Match { value: TextMatch },
}

#[derive(Clone, Debug, Deserialize, Eq, JsonSchema, PartialEq, Serialize)]
pub struct SearchResult {
    pub items: Vec<SearchItem>,
    pub truncated: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cursor: Option<ContinuationCursor>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub snapshot_revision: Option<FileRevision>,
}

impl Validate for ReadRequest {
    fn validate(&self) -> Result<(), ValidationError> {
        let mut issues = Vec::new();
        append_nested(&mut issues, "read.path", self.path.validate());
        if let Some(page) = &self.page {
            if page.start_line == Some(0) {
                issue(&mut issues, "read.page.start_line", "must be one-based");
            }
            if page.max_lines == 0 {
                issue(
                    &mut issues,
                    "read.page.max_lines",
                    "must be greater than zero",
                );
            }
            if page.max_bytes == 0 {
                issue(
                    &mut issues,
                    "read.page.max_bytes",
                    "must be greater than zero",
                );
            }
            if page.max_line_bytes == 0 {
                issue(
                    &mut issues,
                    "read.page.max_line_bytes",
                    "must be greater than zero",
                );
            }
            if page.start_line.is_some() && page.cursor.is_some() {
                issue(
                    &mut issues,
                    "read.page",
                    "start_line and cursor are mutually exclusive",
                );
            }
        }
        finish(issues)
    }
}

impl Validate for ListRequest {
    fn validate(&self) -> Result<(), ValidationError> {
        let mut issues = Vec::new();
        append_nested(&mut issues, "list.path", self.path.validate());
        if self.limit == 0 {
            issue(&mut issues, "list.limit", "must be greater than zero");
        }
        finish(issues)
    }
}

impl Validate for SearchRequest {
    fn validate(&self) -> Result<(), ValidationError> {
        let mut issues = Vec::new();
        append_nested(&mut issues, "search.root", self.root.validate());
        if self.pattern.is_empty() && self.kind == SearchKind::Content {
            issue(
                &mut issues,
                "search.pattern",
                "must not be empty for content searches",
            );
        }
        if self.max_results == 0 {
            issue(
                &mut issues,
                "search.max_results",
                "must be greater than zero",
            );
        }
        if self.max_bytes == 0 {
            issue(&mut issues, "search.max_bytes", "must be greater than zero");
        }
        if self.max_line_bytes == 0 {
            issue(
                &mut issues,
                "search.max_line_bytes",
                "must be greater than zero",
            );
        }
        finish(issues)
    }
}

const fn default_true() -> bool {
    true
}
