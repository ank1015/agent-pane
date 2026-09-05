//! Administrative catalogue writes and credential-free fleet observations.
use super::{
    RuntimeService,
    error::{Result, RuntimeError},
    model::{limit, nonempty},
    queries,
};
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};
use uuid::Uuid;

#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub(super) struct Harness {
    pub name: String,
    #[serde(default)]
    pub description: Option<String>,
    #[serde(default)]
    pub default_config: Map<String, Value>,
    #[serde(default)]
    pub config_schema: Option<Map<String, Value>>,
    #[serde(default)]
    pub supported_models: std::collections::BTreeMap<String, Vec<String>>,
    #[serde(default = "enabled")]
    pub enabled: bool,
}
fn enabled() -> bool {
    true
}
fn validate_id(id: &str) -> Result<()> {
    if !platform_runtime_contracts::is_valid_harness_id(id) {
        return Err(RuntimeError::Invalid(
            "Harness ids must match [a-z][a-z0-9_-]{0,127}.",
        ));
    }
    Ok(())
}
impl Harness {
    fn validate(&self) -> Result<()> {
        if self.supported_models.len() > 64
            || serde_json::to_vec(&self.supported_models)
                .map_err(|_| RuntimeError::StoredData)?
                .len()
                > 65536
        {
            return Err(RuntimeError::Invalid(
                "supported_models must be at most 64 KiB and contain at most 64 providers.",
            ));
        }
        for (provider, models) in &self.supported_models {
            if !platform_runtime_contracts::is_valid_harness_id(provider) || models.len() > 1000 {
                return Err(RuntimeError::Invalid(
                    "Invalid supported_models provider or too many models.",
                ));
            }
            let mut seen = std::collections::HashSet::new();
            for model in models {
                nonempty(
                    model,
                    512,
                    "Model IDs must contain 1–512 characters without surrounding whitespace.",
                )?;
                if model.chars().any(char::is_control) || !seen.insert(model) {
                    return Err(RuntimeError::Invalid(
                        "Model IDs must be unique per provider and contain no control characters.",
                    ));
                }
            }
        }
        nonempty(
            &self.name,
            128,
            "Provide a name of 1–128 characters without surrounding whitespace.",
        )?;
        if self
            .description
            .as_ref()
            .is_some_and(|s| s.chars().count() > 8192)
        {
            return Err(RuntimeError::Invalid(
                "description must be at most 8192 characters.",
            ));
        }
        for value in [Some(&self.default_config), self.config_schema.as_ref()]
            .into_iter()
            .flatten()
        {
            if serde_json::to_vec(value)
                .map_err(|_| RuntimeError::StoredData)?
                .len()
                > 65536
            {
                return Err(RuntimeError::Invalid(
                    "Configuration and schema must each be at most 64 KiB.",
                ));
            }
        }
        if let Some(schema) = &self.config_schema {
            jsonschema::draft202012::new(&Value::Object(schema.clone())).map_err(|_| {
                RuntimeError::Invalid(
                    "Provide a valid, locally resolvable JSON Schema (draft 2020-12).",
                )
            })?;
        }
        // Defaults may be partial: required properties can arrive in run overrides.
        Ok(())
    }
}

#[derive(Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct WorkerQuery {
    pub limit: Option<u32>,
    pub cursor: Option<String>,
    pub status: Option<String>,
    pub harness_id: Option<String>,
}

// Credential hashes live in worker_credentials and must never be joined here.
const WORKER_VIEW: &str = "to_jsonb(w) || jsonb_build_object('active_assignments', a.active, 'available_capacity', case when w.status='accepting' then greatest(w.capacity-a.active,0) else 0 end, 'stale', w.last_seen_at < clock_timestamp()-interval '120 seconds')";
const ASSIGNMENTS: &str = "left join lateral (select count(*) as active from runs r where r.worker_id=w.id and r.status='running' and r.lease_expires_at>clock_timestamp()) a on true";

impl RuntimeService {
    pub(super) async fn put_harness(&self, id: &str, request: Harness) -> Result<Value> {
        validate_id(id)?;
        request.validate()?;
        let mut tx = self.transaction().await?;
        let result = sqlx::query_scalar("insert into harnesses as h(id,name,description,default_config,config_schema,enabled,supported_models) values($1,$2,$3,$4,$5,$6,$7) on conflict(id) do update set name=excluded.name,description=excluded.description,default_config=excluded.default_config,config_schema=excluded.config_schema,enabled=excluded.enabled,supported_models=excluded.supported_models,updated_at=clock_timestamp() returning to_jsonb(h)")
            .bind(id).bind(request.name).bind(request.description).bind(Value::Object(request.default_config))
            .bind(request.config_schema.map(Value::Object)).bind(request.enabled).bind(serde_json::to_value(request.supported_models).map_err(|_| RuntimeError::StoredData)?).fetch_one(&mut *tx).await?;
        tx.commit().await?;
        Ok(result)
    }

    pub(super) async fn patch_harness(&self, id: &str, patch: Map<String, Value>) -> Result<Value> {
        validate_id(id)?;
        if patch.is_empty()
            || patch.keys().any(|k| {
                !matches!(
                    k.as_str(),
                    "name"
                        | "description"
                        | "default_config"
                        | "config_schema"
                        | "enabled"
                        | "supported_models"
                )
            })
        {
            return Err(RuntimeError::Invalid(
                "Provide one or more supported harness fields.",
            ));
        }
        let mut tx = self.transaction().await?;
        let mut current: Value = sqlx::query_scalar("select jsonb_build_object('name',name,'description',description,'default_config',default_config,'config_schema',config_schema,'enabled',enabled,'supported_models',supported_models) from harnesses where id=$1 for update")
            .bind(id).fetch_optional(&mut *tx).await?.ok_or(RuntimeError::NotFound)?;
        current
            .as_object_mut()
            .ok_or(RuntimeError::StoredData)?
            .extend(patch);
        let request: Harness = serde_json::from_value(current)
            .map_err(|_| RuntimeError::Invalid("Invalid harness field type."))?;
        request.validate()?;
        let result = sqlx::query_scalar("update harnesses as h set name=$2,description=$3,default_config=$4,config_schema=$5,enabled=$6,supported_models=$7,updated_at=clock_timestamp() where id=$1 returning to_jsonb(h)")
            .bind(id).bind(request.name).bind(request.description).bind(Value::Object(request.default_config))
            .bind(request.config_schema.map(Value::Object)).bind(request.enabled).bind(serde_json::to_value(request.supported_models).map_err(|_| RuntimeError::StoredData)?).fetch_one(&mut *tx).await?;
        tx.commit().await?;
        Ok(result)
    }

    pub(super) async fn admin_workers(&self, q: WorkerQuery) -> Result<Value> {
        let count = limit(q.limit)?;
        if q.status
            .as_deref()
            .is_some_and(|s| !matches!(s, "accepting" | "draining" | "offline"))
        {
            return Err(RuntimeError::Invalid("Invalid worker status."));
        }
        if let Some(harness) = &q.harness_id {
            validate_id(harness)?;
        }
        let tag = format!("workers:{:?}:{:?}", q.status, q.harness_id);
        let cursor = queries::cursor(q.cursor.as_deref(), &tag)?;
        let items = sqlx::query_scalar(&format!("select {WORKER_VIEW} from workers w {ASSIGNMENTS} where ($1::text is null or w.status=$1) and ($2::text is null or $2=any(w.supported_harnesses)) and ($3::timestamptz is null or (w.started_at,w.id)<($3,$4)) order by w.started_at desc,w.id desc limit $5"))
            .bind(q.status).bind(q.harness_id).bind(cursor.as_ref().map(|c| c.at)).bind(cursor.map(|c|c.id))
            .bind(count+1).fetch_all(&self.pool).await?;
        queries::page(items, count, tag, "started_at")
    }

    pub(super) async fn admin_worker(&self, id: Uuid) -> Result<Value> {
        sqlx::query_scalar(&format!(
            "select {WORKER_VIEW} from workers w {ASSIGNMENTS} where w.id=$1"
        ))
        .bind(id)
        .fetch_optional(&self.pool)
        .await?
        .ok_or(RuntimeError::NotFound)
    }
}
