use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::{
    GrantId, Validate, ValidationError, ValidationIssue, WorkspaceRootId,
    validation::{finish, issue, require_non_empty},
};

/// A path resolved by the target machine.
#[derive(Clone, Debug, Deserialize, Eq, JsonSchema, PartialEq, Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum PathSpec {
    /// Portable path relative to a configured workspace root.
    Workspace {
        root_id: WorkspaceRootId,
        /// Slash-separated path. `.` addresses the workspace root.
        path: String,
    },
    /// Target-native URI authorized by an explicit grant.
    Native { uri: String, grant_id: GrantId },
}

impl PathSpec {
    #[must_use]
    pub fn workspace(root_id: WorkspaceRootId, path: impl Into<String>) -> Self {
        Self::Workspace {
            root_id,
            path: path.into(),
        }
    }
}

impl Validate for PathSpec {
    fn validate(&self) -> Result<(), ValidationError> {
        let mut issues = Vec::new();
        match self {
            Self::Workspace { path, .. } => validate_workspace_path(path, &mut issues),
            Self::Native { uri, .. } => {
                require_non_empty(&mut issues, "path.uri", uri);
                if !uri.contains("://") {
                    issue(
                        &mut issues,
                        "path.uri",
                        "must be an absolute target-native URI",
                    );
                }
            }
        }
        finish(issues)
    }
}

/// A workspace root advertised by an execution machine.
#[derive(Clone, Debug, Deserialize, Eq, JsonSchema, PartialEq, Serialize)]
pub struct WorkspaceRoot {
    pub id: WorkspaceRootId,
    pub name: String,
    /// Target-native URI used for display and diagnostics.
    pub uri: String,
    #[serde(default)]
    pub read_only: bool,
}

impl Validate for WorkspaceRoot {
    fn validate(&self) -> Result<(), ValidationError> {
        let mut issues = Vec::new();
        require_non_empty(&mut issues, "workspace_root.name", &self.name);
        require_non_empty(&mut issues, "workspace_root.uri", &self.uri);
        if !self.uri.contains("://") {
            issue(
                &mut issues,
                "workspace_root.uri",
                "must be an absolute target-native URI",
            );
        }
        finish(issues)
    }
}

fn validate_workspace_path(path: &str, issues: &mut Vec<ValidationIssue>) {
    require_non_empty(issues, "path.path", path);
    if path.contains('\0') {
        issue(issues, "path.path", "must not contain a null byte");
    }
    if path.starts_with('/') || path.starts_with('\\') {
        issue(
            issues,
            "path.path",
            "must be relative to its workspace root",
        );
    }
    if path.contains('\\') {
        issue(issues, "path.path", "must use forward slashes");
    }
    if path.len() >= 2 && path.as_bytes()[1] == b':' {
        issue(issues, "path.path", "must not contain a drive prefix");
    }
    if path.split('/').any(|segment| segment == "..") {
        issue(
            issues,
            "path.path",
            "must not traverse above its workspace root",
        );
    }
}
