use chrono::{DateTime, Utc};
use execution_contracts::{EnvironmentId, MachineId, TimestampMs, WorkspaceRootId};
use execution_protocol::Environment;
use serde::{Deserialize, Serialize};
use sqlx::Row;
use uuid::Uuid;

use crate::{
    db::{Database, DbError},
    sandbox_accounts::SandboxProvider,
};

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct SandboxEnvironmentTemplate {
    pub id: Uuid,
    pub name: String,
    pub snapshot_id: Uuid,
    pub sandbox_account_id: Uuid,
    pub provider: SandboxProvider,
    pub cwd: String,
    pub creation_script: String,
    pub created_at: TimestampMs,
    pub updated_at: TimestampMs,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct CreateSandboxEnvironmentTemplateRequest {
    pub name: String,
    pub snapshot_id: Uuid,
    pub cwd: String,
    #[serde(default)]
    pub creation_script: String,
}

#[derive(Clone, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct UpdateSandboxEnvironmentTemplateRequest {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub snapshot_id: Option<Uuid>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cwd: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub creation_script: Option<String>,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct SandboxEnvironmentInstance {
    pub template_id: Uuid,
    pub provider: SandboxProvider,
    pub provider_sandbox_id: String,
    pub environment: Environment,
    pub created_at: TimestampMs,
}

impl Database {
    pub async fn create_sandbox_environment_template(
        &self,
        id: Uuid,
        request: &CreateSandboxEnvironmentTemplateRequest,
    ) -> Result<Option<SandboxEnvironmentTemplate>, DbError> {
        let inserted = sqlx::query(
            "insert into sandbox_environment_templates
                 (id, name, snapshot_id, cwd, creation_script)
             select $1, $2, s.id, $4, $5
             from snapshots s where s.id = $3
             returning id",
        )
        .bind(id)
        .bind(&request.name)
        .bind(request.snapshot_id)
        .bind(&request.cwd)
        .bind(&request.creation_script)
        .fetch_optional(self.pool())
        .await?;
        if inserted.is_none() {
            return Ok(None);
        }
        self.sandbox_environment_template(id).await
    }

    pub async fn sandbox_environment_template(
        &self,
        id: Uuid,
    ) -> Result<Option<SandboxEnvironmentTemplate>, DbError> {
        sqlx::query(&format!(
            "{TEMPLATE_SELECT} where t.id = $1 and t.deleted_at is null"
        ))
        .bind(id)
        .fetch_optional(self.pool())
        .await?
        .map(template_from_row)
        .transpose()
    }

    pub async fn sandbox_environment_templates(
        &self,
    ) -> Result<Vec<SandboxEnvironmentTemplate>, DbError> {
        sqlx::query(&format!(
            "{TEMPLATE_SELECT} where t.deleted_at is null order by lower(t.name), t.created_at, t.id"
        ))
        .fetch_all(self.pool())
        .await?
        .into_iter()
        .map(template_from_row)
        .collect()
    }

    pub async fn update_sandbox_environment_template(
        &self,
        id: Uuid,
        request: &UpdateSandboxEnvironmentTemplateRequest,
    ) -> Result<Option<SandboxEnvironmentTemplate>, DbError> {
        let updated = sqlx::query(
            "update sandbox_environment_templates t
             set name = coalesce($2, t.name),
                 snapshot_id = coalesce($3, t.snapshot_id),
                 cwd = coalesce($4, t.cwd),
                 creation_script = coalesce($5, t.creation_script)
             where t.id = $1 and t.deleted_at is null
               and ($3::uuid is null or exists(select 1 from snapshots s where s.id = $3))
             returning t.id",
        )
        .bind(id)
        .bind(request.name.as_deref())
        .bind(request.snapshot_id)
        .bind(request.cwd.as_deref())
        .bind(request.creation_script.as_deref())
        .fetch_optional(self.pool())
        .await?;
        if updated.is_none() {
            return Ok(None);
        }
        self.sandbox_environment_template(id).await
    }

    pub async fn delete_sandbox_environment_template(&self, id: Uuid) -> Result<bool, DbError> {
        sqlx::query(
            "update sandbox_environment_templates set deleted_at = now()
             where id = $1 and deleted_at is null",
        )
        .bind(id)
        .execute(self.pool())
        .await
        .map(|result| result.rows_affected() == 1)
        .map_err(Into::into)
    }

    pub async fn sandbox_environment_instances(
        &self,
        template_id: Uuid,
    ) -> Result<Vec<SandboxEnvironmentInstance>, DbError> {
        sqlx::query(
            "select i.template_id, a.provider, sm.provider_resource_id,
                    e.environment_id, e.machine_id, e.name as environment_name,
                    e.workspace_root_id, e.path,
                    e.created_at as environment_created_at, i.created_at
             from sandbox_environment_instances i
             join environments e on e.environment_id = i.environment_id
             join sandbox_machines sm on sm.machine_id = e.machine_id
             join sandbox_accounts a on a.id = sm.sandbox_account_id
             where i.template_id = $1 and e.deleted_at is null
             order by i.created_at, i.environment_id",
        )
        .bind(template_id)
        .fetch_all(self.pool())
        .await?
        .into_iter()
        .map(instance_from_row)
        .collect()
    }
}

const TEMPLATE_SELECT: &str =
    "select t.id, t.name, t.snapshot_id, s.sandbox_account_id, a.provider,
            t.cwd, t.creation_script, t.created_at, t.updated_at
     from sandbox_environment_templates t
     join snapshots s on s.id = t.snapshot_id
     join sandbox_accounts a on a.id = s.sandbox_account_id";

pub(crate) fn validate_create(
    request: &mut CreateSandboxEnvironmentTemplateRequest,
) -> Result<(), &'static str> {
    validate_name(&request.name)?;
    request.cwd = normalize_cwd(&request.cwd)?;
    Ok(())
}

pub(crate) fn validate_update(
    request: &mut UpdateSandboxEnvironmentTemplateRequest,
) -> Result<(), &'static str> {
    if request.name.is_none()
        && request.snapshot_id.is_none()
        && request.cwd.is_none()
        && request.creation_script.is_none()
    {
        return Err("at least one field must be supplied");
    }
    if let Some(name) = &request.name {
        validate_name(name)?;
    }
    if let Some(cwd) = &request.cwd {
        request.cwd = Some(normalize_cwd(cwd)?);
    }
    Ok(())
}

fn validate_name(name: &str) -> Result<(), &'static str> {
    if name.is_empty() || name != name.trim() || name.chars().count() > 120 {
        return Err("name must be 1-120 characters without surrounding whitespace");
    }
    Ok(())
}

pub(crate) fn normalize_cwd(cwd: &str) -> Result<String, &'static str> {
    if !cwd.starts_with('/') || cwd.contains('\0') || cwd.contains('\\') {
        return Err("cwd must be an absolute POSIX path");
    }
    let mut parts = Vec::new();
    for part in cwd.split('/') {
        match part {
            "" | "." => {}
            ".." => return Err("cwd must not contain parent traversal"),
            value => parts.push(value),
        }
    }
    Ok(if parts.is_empty() {
        "/".to_owned()
    } else {
        format!("/{}", parts.join("/"))
    })
}

pub(crate) fn environment_path(cwd: &str) -> String {
    cwd.strip_prefix('/')
        .filter(|path| !path.is_empty())
        .unwrap_or(".")
        .to_owned()
}

fn template_from_row(row: sqlx::postgres::PgRow) -> Result<SandboxEnvironmentTemplate, DbError> {
    Ok(SandboxEnvironmentTemplate {
        id: row.try_get("id")?,
        name: row.try_get("name")?,
        snapshot_id: row.try_get("snapshot_id")?,
        sandbox_account_id: row.try_get("sandbox_account_id")?,
        provider: parse_provider(&row)?,
        cwd: row.try_get("cwd")?,
        creation_script: row.try_get("creation_script")?,
        created_at: timestamp(row.try_get("created_at")?),
        updated_at: timestamp(row.try_get("updated_at")?),
    })
}

fn instance_from_row(row: sqlx::postgres::PgRow) -> Result<SandboxEnvironmentInstance, DbError> {
    let environment = Environment {
        environment_id: EnvironmentId::new(row.try_get::<String, _>("environment_id")?)
            .map_err(|error| DbError::Contract(error.to_string()))?,
        machine_id: MachineId::new(row.try_get::<String, _>("machine_id")?)
            .map_err(|error| DbError::Contract(error.to_string()))?,
        name: row.try_get("environment_name")?,
        workspace_root_id: WorkspaceRootId::new(row.try_get::<String, _>("workspace_root_id")?)
            .map_err(|error| DbError::Contract(error.to_string()))?,
        path: row.try_get("path")?,
        created_at: timestamp(row.try_get("environment_created_at")?),
    };
    Ok(SandboxEnvironmentInstance {
        template_id: row.try_get("template_id")?,
        provider: parse_provider(&row)?,
        provider_sandbox_id: row.try_get("provider_resource_id")?,
        environment,
        created_at: timestamp(row.try_get("created_at")?),
    })
}

fn parse_provider(row: &sqlx::postgres::PgRow) -> Result<SandboxProvider, DbError> {
    row.try_get::<String, _>("provider")?.parse().map_err(
        |error: crate::sandbox_accounts::SandboxAccountError| DbError::Contract(error.to_string()),
    )
}

fn timestamp(value: DateTime<Utc>) -> TimestampMs {
    TimestampMs(u64::try_from(value.timestamp_millis()).unwrap_or(0))
}

#[cfg(test)]
mod tests {
    use super::{environment_path, normalize_cwd};

    #[test]
    fn normalizes_absolute_template_directories() {
        assert_eq!(
            normalize_cwd("/workspace/./app/"),
            Ok("/workspace/app".to_owned())
        );
        assert_eq!(environment_path("/workspace/app"), "workspace/app");
        assert_eq!(environment_path("/"), ".");
        assert!(normalize_cwd("workspace").is_err());
        assert!(normalize_cwd("/workspace/../secret").is_err());
    }
}
