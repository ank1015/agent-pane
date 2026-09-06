//! Project admission policy, independent of worker execution/recovery.
use super::{
    RuntimeService,
    error::{Result, RuntimeError},
};
use platform_runtime_contracts::ConflictCode;
use serde::Deserialize;
use serde_json::{Value, json};
use sqlx::{Postgres, Transaction};
use uuid::Uuid;

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct SetProjectHarness {
    pub enabled: bool,
}

// Shared project locks admit concurrent new work. The setter holds an exclusive
// project lock, including when no grant row exists. Neither path holds these
// locks across network calls. Setter never locks sessions/runs, preventing a
// lock-order inversion with existing session -> run -> admission transactions.
pub(super) async fn require_enabled(
    tx: &mut Transaction<'_, Postgres>,
    project: Uuid,
    harness: &str,
) -> Result<()> {
    validate_id(harness)?;
    sqlx::query_scalar::<_, Uuid>("select project_id from projects where project_id=$1 for share")
        .bind(project)
        .fetch_optional(&mut **tx)
        .await?
        .ok_or(RuntimeError::NotFound)?;
    let (global, policy): (bool, String) =
        sqlx::query_as("select enabled,project_policy from harnesses where id=$1 for share")
            .bind(harness)
            .fetch_optional(&mut **tx)
            .await?
            .ok_or(RuntimeError::NotFound)?;
    if !global {
        return Err(RuntimeError::CodedConflict(
            ConflictCode::HarnessDisabled,
            "The harness is disabled for new work.",
        ));
    }
    let enabled = policy == "required" || sqlx::query_scalar::<_, bool>("select exists(select 1 from project_harnesses where project_id=$1 and harness_id=$2 and enabled)")
        .bind(project).bind(harness).fetch_one(&mut **tx).await?;
    if !enabled {
        return Err(RuntimeError::CodedConflict(
            ConflictCode::ProjectHarnessDisabled,
            "Enable this harness in the project before creating sessions or starting runs.",
        ));
    }
    Ok(())
}

fn validate_id(id: &str) -> Result<()> {
    if !platform_runtime_contracts::is_valid_harness_id(id) {
        return Err(RuntimeError::Invalid("Provide a valid harness ID."));
    }
    Ok(())
}

const VIEW: &str = "to_jsonb(h) || jsonb_build_object('globally_enabled',h.enabled,'project_enabled',(h.project_policy='required' or coalesce(p.enabled,false)),'available',(h.enabled and (h.project_policy='required' or coalesce(p.enabled,false))))";

impl RuntimeService {
    pub(super) async fn project_harnesses(&self, project: Uuid) -> Result<Value> {
        let mut tx = self.transaction().await?;
        sqlx::query_scalar::<_, Uuid>(
            "select project_id from projects where project_id=$1 for key share",
        )
        .bind(project)
        .fetch_optional(&mut *tx)
        .await?
        .ok_or(RuntimeError::NotFound)?;
        let items: Vec<Value> = sqlx::query_scalar(&format!("select {VIEW} from harnesses h left join project_harnesses p on p.harness_id=h.id and p.project_id=$1 order by h.name,h.id"))
            .bind(project).fetch_all(&mut *tx).await?;
        tx.commit().await?;
        Ok(json!({"items":items}))
    }

    pub(super) async fn set_project_harness(
        &self,
        project: Uuid,
        harness: &str,
        request: SetProjectHarness,
    ) -> Result<Value> {
        validate_id(harness)?;
        let mut tx = self.transaction().await?;
        sqlx::query_scalar::<_, Uuid>(
            "select project_id from projects where project_id=$1 for update",
        )
        .bind(project)
        .fetch_optional(&mut *tx)
        .await?
        .ok_or(RuntimeError::NotFound)?;
        let policy: String =
            sqlx::query_scalar("select project_policy from harnesses where id=$1 for share")
                .bind(harness)
                .fetch_optional(&mut *tx)
                .await?
                .ok_or(RuntimeError::NotFound)?;
        if policy == "required" {
            if !request.enabled {
                return Err(RuntimeError::CodedConflict(
                    ConflictCode::ProjectHarnessRequired,
                    "This platform harness is required and cannot be disabled by a project.",
                ));
            }
        } else {
            if !request.enabled {
                let active: bool = sqlx::query_scalar("select exists(select 1 from sessions s join runs r on r.session_id=s.id where s.project_id=$1 and s.harness_id=$2 and r.status in ('ready','running','waiting'))")
                    .bind(project).bind(harness).fetch_one(&mut *tx).await?;
                if active {
                    return Err(RuntimeError::CodedConflict(
                        ConflictCode::ProjectHarnessActiveRuns,
                        "Finish or abort this project's queued, running, and waiting runs before disabling the harness.",
                    ));
                }
            }
            sqlx::query("insert into project_harnesses as p(project_id,harness_id,enabled) values($1,$2,$3) on conflict(project_id,harness_id) do update set enabled=excluded.enabled,updated_at=clock_timestamp() where p.enabled is distinct from excluded.enabled")
                .bind(project).bind(harness).bind(request.enabled).execute(&mut *tx).await?;
        }
        let result = sqlx::query_scalar(&format!("select {VIEW} from harnesses h left join project_harnesses p on p.harness_id=h.id and p.project_id=$1 where h.id=$2"))
            .bind(project).bind(harness).fetch_one(&mut *tx).await?;
        tx.commit().await?;
        Ok(result)
    }
}
