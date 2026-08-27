use llm_contracts::JsonObject;
use serde_json::Value;
use sqlx::{Postgres, Transaction};

use super::{ExecutionError, HarnessSelection, records::RevisionConfigurationRow};

pub(super) struct ResolvedConfiguration {
    pub harness_revision_id: String,
    pub value: JsonObject,
}

pub(super) async fn resolve_for_start(
    transaction: &mut Transaction<'_, Postgres>,
    selection: &HarnessSelection,
    config_override: &JsonObject,
) -> Result<ResolvedConfiguration, ExecutionError> {
    let row = match selection {
        HarnessSelection::ActiveRevision { harness_id } => {
            sqlx::query_as::<_, RevisionConfigurationRow>(
                "select r.harness_revision_id, r.harness_id, r.default_config, r.config_schema \
                 from harnesses h \
                 join harness_revisions r \
                   on r.harness_id = h.harness_id \
                  and r.harness_revision_id = h.active_revision_id \
                 where h.harness_id = $1 and h.enabled and r.retired_at is null \
                 for share of h, r",
            )
            .bind(harness_id)
            .fetch_optional(&mut **transaction)
            .await?
            .ok_or_else(|| ExecutionError::HarnessNotAvailable(harness_id.clone()))?
        }
        HarnessSelection::ExactRevision {
            harness_revision_id,
        } => sqlx::query_as::<_, RevisionConfigurationRow>(
            "select r.harness_revision_id, r.harness_id, r.default_config, r.config_schema \
             from harness_revisions r \
             join harnesses h on h.harness_id = r.harness_id \
             where r.harness_revision_id = $1 and h.enabled \
               and r.first_activated_at is not null and r.retired_at is null \
             for share of h, r",
        )
        .bind(harness_revision_id)
        .fetch_optional(&mut **transaction)
        .await?
        .ok_or_else(|| ExecutionError::HarnessRevisionNotAvailable(harness_revision_id.clone()))?,
    };
    resolve(row, config_override)
}

fn resolve(
    row: RevisionConfigurationRow,
    config_override: &JsonObject,
) -> Result<ResolvedConfiguration, ExecutionError> {
    let value = merge(row.default_config.0, config_override);
    validate(row.config_schema.as_ref().map(|schema| &schema.0), &value)?;
    Ok(ResolvedConfiguration {
        harness_revision_id: row.harness_revision_id,
        value,
    })
}

fn merge(default_config: JsonObject, config_override: &JsonObject) -> JsonObject {
    let mut target = Value::Object(default_config);
    merge_patch(&mut target, &Value::Object(config_override.clone()));
    target
        .as_object()
        .cloned()
        .expect("an object merge patch over an object remains an object")
}

fn merge_patch(target: &mut Value, patch: &Value) {
    let Value::Object(patch) = patch else {
        *target = patch.clone();
        return;
    };
    if !target.is_object() {
        *target = Value::Object(JsonObject::new());
    }
    let target = target.as_object_mut().expect("target was made an object");
    for (key, value) in patch {
        if value.is_null() {
            target.remove(key);
        } else {
            merge_patch(target.entry(key.clone()).or_insert(Value::Null), value);
        }
    }
}

fn validate(schema: Option<&JsonObject>, configuration: &JsonObject) -> Result<(), ExecutionError> {
    let Some(schema) = schema else {
        return Ok(());
    };
    let schema = Value::Object(schema.clone());
    let validator = jsonschema::draft202012::new(&schema).map_err(|error| {
        ExecutionError::InvalidStoredData(format!("stored config_schema is invalid: {error}"))
    })?;
    let configuration = Value::Object(configuration.clone());
    if let Err(error) = validator.validate(&configuration) {
        return Err(ExecutionError::InvalidHarnessConfiguration(format!(
            "{} at {}",
            error,
            error.instance_path()
        )));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use llm_contracts::JsonObject;
    use serde_json::{Value, json};

    use super::{merge, validate};
    use crate::execution::ExecutionError;

    #[test]
    fn recursively_merges_configuration_and_removes_null_keys() {
        let default = object(json!({
            "model": "gpt-5",
            "sampling": {"temperature": 1.0, "top_p": 0.9},
            "removed": true
        }));
        let config_override = object(json!({
            "sampling": {"temperature": 0.2},
            "removed": null
        }));

        assert_eq!(
            merge(default, &config_override),
            object(json!({
                "model": "gpt-5",
                "sampling": {"temperature": 0.2, "top_p": 0.9}
            }))
        );
    }

    #[test]
    fn validates_the_resolved_configuration() {
        let schema = object(json!({
            "type": "object",
            "properties": {"turns": {"type": "integer", "minimum": 1}},
            "required": ["turns"]
        }));
        assert!(validate(Some(&schema), &object(json!({"turns": 2}))).is_ok());
        assert!(matches!(
            validate(Some(&schema), &object(json!({"turns": 0}))),
            Err(ExecutionError::InvalidHarnessConfiguration(_))
        ));
    }

    fn object(value: Value) -> JsonObject {
        value.as_object().expect("object").clone()
    }
}
