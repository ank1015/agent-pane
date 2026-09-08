//! Environment records are references, not live sandbox instances.
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub enum EnvironmentType {
    Machine,
    Sandbox,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct CreateEnvironment {
    pub name: String,
    #[serde(rename = "type")]
    pub kind: EnvironmentType,
    pub machine_id: Option<Uuid>,
    pub snapshot_id: Option<Uuid>,
    /// Absolute native workspace path, as advertised by the execution host.
    pub workspace_root: String,
    /// Portable root-relative directory; `.` selects the root itself.
    pub path: String,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct Environment {
    pub id: Uuid,
    pub project_id: Uuid,
    pub name: String,
    #[serde(rename = "type")]
    pub kind: EnvironmentType,
    pub machine_id: Option<Uuid>,
    pub snapshot_id: Option<Uuid>,
    pub workspace_root: String,
    pub path: String,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

/// Validate host-native paths without interpreting them using the server's OS.
pub fn is_absolute_workspace_root(value: &str) -> bool {
    let bytes = value.as_bytes();
    if value.trim() != value || value.len() > 4096 || value.chars().any(char::is_control) {
        return false;
    }
    let absolute = value.starts_with('/')
        || (bytes.len() >= 3
            && bytes[0].is_ascii_alphabetic()
            && bytes[1] == b':'
            && matches!(bytes[2], b'/' | b'\\'))
        || (value.starts_with("\\\\")
            && value[2..]
                .split(['/', '\\'])
                .filter(|part| !part.is_empty())
                .count()
                >= 2);
    absolute
        && !value
            .split(['/', '\\'])
            .any(|part| matches!(part, "." | ".."))
}

#[cfg(test)]
mod tests {
    use super::is_absolute_workspace_root;

    #[test]
    fn native_workspace_paths_are_cross_platform_not_ids() {
        for path in [
            "/",
            "/home/user",
            "/Users/Some User/work",
            r"C:\",
            r"D:\projects",
            r"\\server\share\workspace",
            "C:/work",
        ] {
            assert!(is_absolute_workspace_root(path), "{path}");
        }
        for path in [
            "",
            "workspace",
            "root",
            "C:work",
            r"\work",
            r"\\server",
            "/work/../other",
            "/work/./other",
            " /work",
            "/work\n",
        ] {
            assert!(!is_absolute_workspace_root(path), "{path}");
        }
    }
}
