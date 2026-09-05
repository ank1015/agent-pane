//! Environment records are references, not live sandbox instances.
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EnvironmentType {
    Machine,
    Sandbox,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CreateEnvironment {
    pub name: String,
    #[serde(rename = "type")]
    pub kind: EnvironmentType,
    pub machine_id: Option<Uuid>,
    pub snapshot_id: Option<Uuid>,
    /// Registered root ID, not its native path.
    pub workspace_root: String,
    /// Portable root-relative directory; `.` selects the root itself.
    pub path: String,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Environment {
    pub id: Uuid,
    pub project_id: Uuid,
    pub name: String,
    #[serde(rename = "type")]
    pub kind: EnvironmentType,
    pub machine_id: Option<Uuid>,
    pub snapshot_id: Option<Uuid>,
    pub workspace_root: String,
    pub workspace_root_path: Option<String>,
    pub path: String,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}
