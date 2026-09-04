use super::error::EnvironmentError;
use chrono::{DateTime, Utc};
use execution_core::{ExecutionPath, RootId};
use serde::{Deserialize, Deserializer, Serialize};
use uuid::Uuid;

#[derive(Clone, Copy, Debug, Deserialize, Serialize, PartialEq, Eq, sqlx::Type)]
#[serde(rename_all = "snake_case")]
#[sqlx(type_name = "text", rename_all = "snake_case")]
pub enum EnvironmentType {
    Machine,
    Sandbox,
}

#[derive(Debug, Serialize, sqlx::FromRow)]
pub struct Environment {
    pub id: Uuid,
    pub project_id: Uuid,
    pub name: String,
    #[serde(rename = "type")]
    #[sqlx(rename = "type")]
    pub kind: EnvironmentType,
    pub machine_id: Option<Uuid>,
    pub snapshot_id: Option<Uuid>,
    pub workspace_root: String,
    pub workspace_root_path: Option<String>,
    pub path: String,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CreateEnvironment {
    pub name: String,
    #[serde(rename = "type")]
    pub kind: EnvironmentType,
    pub machine_id: Option<Uuid>,
    pub snapshot_id: Option<Uuid>,
    pub workspace_root: String,
    pub path: String,
}

#[derive(Debug, Deserialize, Default)]
#[serde(deny_unknown_fields)]
pub struct UpdateEnvironment {
    #[serde(default, deserialize_with = "present")]
    pub name: Option<String>,
    #[serde(default, deserialize_with = "present")]
    pub machine_id: Option<Option<Uuid>>,
    #[serde(default, deserialize_with = "present")]
    pub snapshot_id: Option<Option<Uuid>>,
    #[serde(default, deserialize_with = "present")]
    pub workspace_root: Option<String>,
    #[serde(default, deserialize_with = "present")]
    pub path: Option<String>,
}

fn present<'de, D: Deserializer<'de>, T: Deserialize<'de>>(de: D) -> Result<Option<T>, D::Error> {
    T::deserialize(de).map(Some)
}

impl CreateEnvironment {
    pub fn validate(&self) -> Result<(), EnvironmentError> {
        if self.name.trim().is_empty()
            || self.name.trim() != self.name
            || self.name.chars().count() > 128
            || self.name.chars().any(char::is_control)
        {
            return Err(EnvironmentError::Invalid(
                "Name must contain 1–128 characters without surrounding whitespace or control characters.",
            ));
        }
        if !matches!(
            (self.kind, self.machine_id, self.snapshot_id),
            (EnvironmentType::Machine, Some(_), None) | (EnvironmentType::Sandbox, None, Some(_))
        ) {
            return Err(EnvironmentError::Invalid(
                "Machine environments require only machine_id; sandbox environments require only snapshot_id.",
            ));
        }
        if self.workspace_root.trim() != self.workspace_root
            || self.workspace_root.is_empty()
            || self.workspace_root.len() > 128
            || self.workspace_root.chars().any(char::is_control)
        {
            return Err(EnvironmentError::Invalid(
                "workspace_root must be a root ID of 1–128 bytes.",
            ));
        }
        let root = RootId::new(self.workspace_root.clone())
            .map_err(|_| EnvironmentError::Invalid("Invalid workspace root ID."))?;
        if self.path.len() > 4096
            || self.path.chars().any(char::is_control)
            || ExecutionPath::new(root, self.path.clone()).is_err()
        {
            return Err(EnvironmentError::Invalid(
                "path must be a portable relative path of 1–4096 bytes; use '.' for the root and never absolute paths or '..'.",
            ));
        }
        Ok(())
    }
}

impl UpdateEnvironment {
    pub fn merge(self, old: &Environment) -> Result<CreateEnvironment, EnvironmentError> {
        if self.name.is_none()
            && self.machine_id.is_none()
            && self.snapshot_id.is_none()
            && self.workspace_root.is_none()
            && self.path.is_none()
        {
            return Err(EnvironmentError::Invalid(
                "Provide at least one environment field to update.",
            ));
        }
        let merged = CreateEnvironment {
            name: self.name.unwrap_or_else(|| old.name.clone()),
            kind: old.kind,
            machine_id: self.machine_id.unwrap_or(old.machine_id),
            snapshot_id: self.snapshot_id.unwrap_or(old.snapshot_id),
            workspace_root: self
                .workspace_root
                .unwrap_or_else(|| old.workspace_root.clone()),
            path: self.path.unwrap_or_else(|| old.path.clone()),
        };
        merged.validate()?;
        Ok(merged)
    }
}
