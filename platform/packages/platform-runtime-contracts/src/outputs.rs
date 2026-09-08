//! Explicit harness outputs. References describe resources; they do not grant access.
use crate::is_absolute_workspace_root;
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use uuid::Uuid;

pub const RUN_OUTPUT_MAX_BYTES: usize = 64 * 1024;
pub const RUN_OUTPUT_MAX_COUNT: usize = 64;
pub const RUN_OUTPUT_PAGE_BYTES: usize = 192 * 1024;

pub fn valid_output_name(name: &str) -> bool {
    (1..=128).contains(&name.len())
        && name.as_bytes()[0].is_ascii_alphabetic()
        && name
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b"_.-".contains(&b))
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub enum OutputKind {
    ExecutionWorkspace,
    Json,
    Artifact,
}

/// A harness-selected target, which may be shared with other runs in its session.
/// Availability and execution authorization must be checked when using it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct ExecutionWorkspace {
    pub host_id: Uuid,
    pub workspace_root: String,
    /// Portable directory relative to workspace_root; `.` means the root.
    pub path: String,
    /// Optional source environment, checked for project membership at publication.
    pub environment_id: Option<Uuid>,
    /// Optional tracked sandbox lifecycle handle; it does not grant access.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sandbox_id: Option<Uuid>,
}
impl ExecutionWorkspace {
    pub fn validate(&self) -> Result<(), &'static str> {
        if self.host_id.is_nil()
            || !is_absolute_workspace_root(&self.workspace_root)
            || self.path.is_empty()
            || self.path.len() > 4096
            || self.path.starts_with('/')
            || self.path.contains(['\\', ':'])
            || self.path.chars().any(char::is_control)
            || self.path.trim() != self.path
            || (self.path != "."
                && self
                    .path
                    .split('/')
                    .any(|p| p.is_empty() || matches!(p, "." | "..")))
            || self.environment_id.is_some_and(|id| id.is_nil())
            || self.sandbox_id.is_some_and(|id| id.is_nil())
        {
            return Err(
                "Workspace requires a host UUID, absolute native root and portable root-relative path.",
            );
        }
        Ok(())
    }
}

/// Immutable artifact identity and integrity metadata. Storage resolution is a
/// separate capability; this record contains no filesystem path or bearer URL.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct ArtifactReference {
    pub artifact_id: Uuid,
    pub sha256: String,
    pub size_bytes: u64,
    pub media_type: String,
}
impl ArtifactReference {
    pub fn validate(&self) -> Result<(), &'static str> {
        if self.artifact_id.is_nil()
            || self.sha256.len() != 64
            || !self
                .sha256
                .bytes()
                .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
            || self.size_bytes > (1_u64 << 53) - 1
            || self.media_type.len() > 128
            || !self.media_type.contains('/')
            || !self.media_type.bytes().all(|b| b.is_ascii_graphic())
        {
            return Err(
                "Artifact requires a UUID, lowercase SHA-256, JS-safe size and media type.",
            );
        }
        Ok(())
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(
    tag = "kind",
    content = "value",
    rename_all = "snake_case",
    deny_unknown_fields
)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub enum OutputValue {
    ExecutionWorkspace(ExecutionWorkspace),
    Json(Value),
    Artifact(ArtifactReference),
}
impl OutputValue {
    pub fn kind(&self) -> OutputKind {
        match self {
            Self::ExecutionWorkspace(_) => OutputKind::ExecutionWorkspace,
            Self::Json(_) => OutputKind::Json,
            Self::Artifact(_) => OutputKind::Artifact,
        }
    }
    pub fn value(&self) -> Value {
        match self {
            Self::ExecutionWorkspace(v) => {
                serde_json::to_value(v).expect("workspace serialization")
            }
            Self::Json(v) => v.clone(),
            Self::Artifact(v) => serde_json::to_value(v).expect("artifact serialization"),
        }
    }
}

/// One immutable name/value publication. Scope and attribution come from the lease.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct PublishRunOutput {
    pub name: String,
    pub output: OutputValue,
}
impl PublishRunOutput {
    pub fn validate(&self) -> Result<(), &'static str> {
        if !valid_output_name(&self.name)
            || serde_json::to_vec(self)
                .map_err(|_| "Invalid output JSON.")?
                .len()
                > RUN_OUTPUT_MAX_BYTES
        {
            return Err("Output requires a valid name and at most 64 KiB of JSON.");
        }
        match &self.output {
            OutputValue::ExecutionWorkspace(v) => v.validate(),
            OutputValue::Artifact(v) => v.validate(),
            OutputValue::Json(_) => Ok(()),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct RunOutput {
    pub run_id: Uuid,
    pub session_id: Uuid,
    pub project_id: Uuid,
    pub sequence: i64,
    pub name: String,
    pub output: OutputValue,
    pub created_at: DateTime<Utc>,
}

#[derive(Debug, Default, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct RunOutputsQuery {
    pub after_sequence: Option<i64>,
    pub limit: Option<u32>,
}
impl RunOutputsQuery {
    pub fn validate(&self) -> Result<(), &'static str> {
        if self.after_sequence.is_some_and(|n| n < 0)
            || self.limit.is_some_and(|n| !(1..=50).contains(&n))
        {
            return Err("Outputs require a nonnegative after_sequence and limit of 1–50.");
        }
        Ok(())
    }
}

#[derive(Debug, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct RunOutputsPage {
    pub items: Vec<RunOutput>,
    pub next_after_sequence: Option<i64>,
}
