use super::error::{Result, RuntimeError};
use llm_contracts::JsonObject;
use serde_json::Value;
use sqlx::{Postgres, Transaction};

pub(super) async fn resolve(
    tx: &mut Transaction<'_, Postgres>,
    harness: &str,
    overrides: &JsonObject,
) -> Result<Value> {
    resolve_from(tx, harness, overrides, None).await
}

pub(super) async fn resolve_from(
    tx: &mut Transaction<'_, Postgres>,
    harness: &str,
    overrides: &JsonObject,
    inherited: Option<&Value>,
) -> Result<Value> {
    if serde_json::to_vec(overrides)
        .map_err(|_| RuntimeError::StoredData)?
        .len()
        > 64 * 1024
    {
        return Err(RuntimeError::Invalid(
            "Configuration overrides must not exceed 64 KiB.",
        ));
    }
    let (mut config, schema, enabled): (Value, Option<Value>, bool) = sqlx::query_as(
        "select default_config,config_schema,enabled from harnesses where id=$1 for share",
    )
    .bind(harness)
    .fetch_optional(&mut **tx)
    .await?
    .ok_or(RuntimeError::NotFound)?;
    if !enabled {
        return Err(RuntimeError::CodedConflict(
            platform_runtime_contracts::ConflictCode::HarnessDisabled,
            "The harness is disabled for new work.",
        ));
    }
    if let Some(inherited) = inherited {
        config = inherited.clone();
    }
    merge(&mut config, &Value::Object(overrides.clone()));
    if serde_json::to_vec(&config)
        .map_err(|_| RuntimeError::StoredData)?
        .len()
        > 64 * 1024
    {
        return Err(RuntimeError::Invalid(
            "Resolved session configuration must not exceed 64 KiB.",
        ));
    }
    if let Some(schema) = schema {
        // Network resolution is disabled by the dependency's feature set.
        let validator =
            jsonschema::draft202012::new(&schema).map_err(|_| RuntimeError::StoredData)?;
        if !validator.is_valid(&config) {
            return Err(RuntimeError::Configuration);
        }
    }
    Ok(config)
}

fn merge(target: &mut Value, patch: &Value) {
    if let Value::Object(fields) = patch {
        if !target.is_object() {
            *target = Value::Object(Default::default());
        }
        let target = target.as_object_mut().expect("object initialized above");
        for (key, value) in fields {
            if value.is_null() {
                target.remove(key);
            } else {
                merge(target.entry(key.clone()).or_insert(Value::Null), value);
            }
        }
    } else {
        *target = patch.clone();
    }
}
