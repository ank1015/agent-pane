use serde_json::Value;
use sqlx::{Postgres, QueryBuilder, types::Json};

use super::{
    CreateOutcome, HarnessActivation, HarnessError, HarnessRevision, HarnessRevisionStatus, Page,
    RegisterHarnessRevision, RevisionListQuery, SetActiveRevision,
    cursor::{decode, encode},
    definitions, page_limit,
    records::{HARNESS_COLUMNS, HarnessRow, REVISION_COLUMNS, RevisionRow},
};
use crate::db::Database;

pub(super) async fn register(
    database: &Database,
    harness_id: &str,
    command: RegisterHarnessRevision,
) -> Result<CreateOutcome<HarnessRevision>, HarnessError> {
    command.validate()?;
    validate_configuration(&command)?;
    definitions::get(database, harness_id).await?;

    let inserted = sqlx::query_scalar::<_, String>(
        "insert into harness_revisions \
         (harness_revision_id, harness_id, revision, contract_version, \
          default_config, config_schema) \
         values ($1, $2, $3, $4, $5, $6) \
         on conflict (harness_revision_id) do nothing \
         returning harness_revision_id",
    )
    .bind(&command.harness_revision_id)
    .bind(harness_id)
    .bind(&command.revision)
    .bind(i64::from(command.contract_version))
    .bind(Json(command.default_config.clone()))
    .bind(command.config_schema.clone().map(Json))
    .fetch_optional(database.pool())
    .await;

    let inserted = match inserted {
        Ok(inserted) => inserted,
        Err(error) if super::constraint(&error) == Some("harness_revisions_name_unique") => {
            return Err(HarnessError::RevisionNameConflict {
                harness_id: harness_id.to_owned(),
                revision: command.revision,
            });
        }
        Err(error) => return Err(error.into()),
    };
    if inserted.is_some() {
        return Ok(CreateOutcome {
            value: get(database, harness_id, &command.harness_revision_id).await?,
            created: true,
        });
    }

    let existing = get_by_id(database, &command.harness_revision_id)
        .await?
        .ok_or_else(|| HarnessError::RevisionIdConflict(command.harness_revision_id.clone()))?;
    if existing.harness_id != harness_id
        || existing.revision != command.revision
        || existing.contract_version != command.contract_version
        || existing.default_config != command.default_config
        || existing.config_schema != command.config_schema
    {
        return Err(HarnessError::RevisionIdConflict(
            command.harness_revision_id,
        ));
    }
    Ok(CreateOutcome {
        value: existing,
        created: false,
    })
}

pub(super) async fn list(
    database: &Database,
    harness_id: &str,
    query: RevisionListQuery,
) -> Result<Page<HarnessRevision>, HarnessError> {
    definitions::get(database, harness_id).await?;
    let limit = page_limit(query.limit)?;
    let cursor = query.cursor.as_deref().map(decode).transpose()?;

    let mut sql = QueryBuilder::<Postgres>::new("select ");
    sql.push(REVISION_COLUMNS)
        .push(" from harness_revisions r join harnesses h on h.harness_id = r.harness_id")
        .push(" where r.harness_id = ")
        .push_bind(harness_id);
    if let Some(status) = query.status {
        sql.push(" and ").push(status_condition(status));
    }
    if let Some(cursor) = cursor {
        sql.push(" and (r.created_at, r.harness_revision_id) < (")
            .push_bind(cursor.created_at)
            .push(", ")
            .push_bind(cursor.id)
            .push(')');
    }
    sql.push(" order by r.created_at desc, r.harness_revision_id desc limit ")
        .push_bind(i64::from(limit) + 1);

    let mut rows = sql
        .build_query_as::<RevisionRow>()
        .fetch_all(database.pool())
        .await?;
    let has_more = rows.len() > limit as usize;
    rows.truncate(limit as usize);
    let next_cursor = if has_more {
        rows.last()
            .map(|row| encode(row.created_at, &row.harness_revision_id))
            .transpose()?
    } else {
        None
    };
    let items = rows
        .into_iter()
        .map(TryInto::try_into)
        .collect::<Result<Vec<_>, _>>()?;
    Ok(Page { items, next_cursor })
}

pub(super) async fn get(
    database: &Database,
    harness_id: &str,
    revision_id: &str,
) -> Result<HarnessRevision, HarnessError> {
    let query = format!(
        "select {REVISION_COLUMNS} \
         from harness_revisions r join harnesses h on h.harness_id = r.harness_id \
         where r.harness_id = $1 and r.harness_revision_id = $2"
    );
    sqlx::query_as::<_, RevisionRow>(&query)
        .bind(harness_id)
        .bind(revision_id)
        .fetch_optional(database.pool())
        .await?
        .map(TryInto::try_into)
        .transpose()?
        .ok_or_else(|| HarnessError::RevisionNotFound(revision_id.to_owned()))
}

pub(super) async fn activate(
    database: &Database,
    harness_id: &str,
    command: SetActiveRevision,
) -> Result<HarnessActivation, HarnessError> {
    command.validate()?;
    let mut transaction = database.pool().begin().await?;
    lock_harness(&mut transaction, harness_id).await?;

    let revision =
        lock_revision(&mut transaction, harness_id, &command.harness_revision_id).await?;
    if revision.retired_at.is_some() {
        return Err(HarnessError::RetiredRevisionCannotBeActivated);
    }
    sqlx::query(
        "update harness_revisions \
         set first_activated_at = coalesce(first_activated_at, now()) \
         where harness_revision_id = $1",
    )
    .bind(&command.harness_revision_id)
    .execute(&mut *transaction)
    .await?;
    let harness = sqlx::query_as::<_, HarnessRow>(&format!(
        "update harnesses set active_revision_id = $2 \
         where harness_id = $1 returning {HARNESS_COLUMNS}"
    ))
    .bind(harness_id)
    .bind(&command.harness_revision_id)
    .fetch_one(&mut *transaction)
    .await?;
    let active_revision =
        fetch_revision(&mut transaction, harness_id, &command.harness_revision_id).await?;
    transaction.commit().await?;

    Ok(HarnessActivation {
        harness: harness.into(),
        active_revision: active_revision.try_into()?,
    })
}

pub(super) async fn clear_active(
    database: &Database,
    harness_id: &str,
) -> Result<super::Harness, HarnessError> {
    let mut transaction = database.pool().begin().await?;
    let current = lock_harness(&mut transaction, harness_id).await?;
    if current.enabled {
        return Err(HarnessError::EnabledHarnessCannotClearRevision);
    }
    let harness = sqlx::query_as::<_, HarnessRow>(&format!(
        "update harnesses set active_revision_id = null \
         where harness_id = $1 returning {HARNESS_COLUMNS}"
    ))
    .bind(harness_id)
    .fetch_one(&mut *transaction)
    .await?;
    transaction.commit().await?;
    Ok(harness.into())
}

pub(super) async fn retire(
    database: &Database,
    harness_id: &str,
    revision_id: &str,
) -> Result<HarnessRevision, HarnessError> {
    let mut transaction = database.pool().begin().await?;
    let harness = lock_harness(&mut transaction, harness_id).await?;
    lock_revision(&mut transaction, harness_id, revision_id).await?;
    if harness.active_revision_id.as_deref() == Some(revision_id) {
        return Err(HarnessError::ActiveRevisionCannotBeRetired);
    }
    sqlx::query(
        "update harness_revisions set retired_at = coalesce(retired_at, now()) \
         where harness_revision_id = $1",
    )
    .bind(revision_id)
    .execute(&mut *transaction)
    .await?;
    let revision = fetch_revision(&mut transaction, harness_id, revision_id).await?;
    transaction.commit().await?;
    revision.try_into()
}

fn validate_configuration(command: &RegisterHarnessRevision) -> Result<(), HarnessError> {
    let Some(schema) = &command.config_schema else {
        return Ok(());
    };
    let schema = Value::Object(schema.clone());
    if let Some(dialect) = schema.get("$schema").and_then(Value::as_str)
        && dialect != "https://json-schema.org/draft/2020-12/schema"
    {
        return Err(HarnessError::InvalidConfigSchema(
            "only JSON Schema Draft 2020-12 is supported".to_owned(),
        ));
    }
    jsonschema::draft202012::meta::validate(&schema)
        .map_err(|error| HarnessError::InvalidConfigSchema(error.to_string()))?;
    let validator = jsonschema::draft202012::new(&schema)
        .map_err(|error| HarnessError::InvalidConfigSchema(error.to_string()))?;
    let default_config = Value::Object(command.default_config.clone());
    if let Err(error) = validator.validate(&default_config) {
        return Err(HarnessError::DefaultConfigInvalid(format!(
            "{} at {}",
            error,
            error.instance_path()
        )));
    }
    Ok(())
}

fn status_condition(status: HarnessRevisionStatus) -> &'static str {
    match status {
        HarnessRevisionStatus::Registered => {
            "r.retired_at is null and r.first_activated_at is null \
             and h.active_revision_id is distinct from r.harness_revision_id"
        }
        HarnessRevisionStatus::Active => {
            "r.retired_at is null and h.active_revision_id = r.harness_revision_id"
        }
        HarnessRevisionStatus::Deprecated => {
            "r.retired_at is null and r.first_activated_at is not null \
             and h.active_revision_id is distinct from r.harness_revision_id"
        }
        HarnessRevisionStatus::Retired => "r.retired_at is not null",
    }
}

async fn get_by_id(
    database: &Database,
    revision_id: &str,
) -> Result<Option<HarnessRevision>, HarnessError> {
    let query = format!(
        "select {REVISION_COLUMNS} \
         from harness_revisions r join harnesses h on h.harness_id = r.harness_id \
         where r.harness_revision_id = $1"
    );
    sqlx::query_as::<_, RevisionRow>(&query)
        .bind(revision_id)
        .fetch_optional(database.pool())
        .await?
        .map(TryInto::try_into)
        .transpose()
}

async fn lock_harness(
    transaction: &mut sqlx::Transaction<'_, Postgres>,
    harness_id: &str,
) -> Result<HarnessRow, HarnessError> {
    sqlx::query_as::<_, HarnessRow>(&format!(
        "select {HARNESS_COLUMNS} from harnesses where harness_id = $1 for update"
    ))
    .bind(harness_id)
    .fetch_optional(&mut **transaction)
    .await?
    .ok_or_else(|| HarnessError::HarnessNotFound(harness_id.to_owned()))
}

async fn lock_revision(
    transaction: &mut sqlx::Transaction<'_, Postgres>,
    harness_id: &str,
    revision_id: &str,
) -> Result<RevisionRow, HarnessError> {
    let query = format!(
        "select {REVISION_COLUMNS} \
         from harness_revisions r join harnesses h on h.harness_id = r.harness_id \
         where r.harness_id = $1 and r.harness_revision_id = $2 for update of r"
    );
    sqlx::query_as::<_, RevisionRow>(&query)
        .bind(harness_id)
        .bind(revision_id)
        .fetch_optional(&mut **transaction)
        .await?
        .ok_or_else(|| HarnessError::RevisionNotFound(revision_id.to_owned()))
}

async fn fetch_revision(
    transaction: &mut sqlx::Transaction<'_, Postgres>,
    harness_id: &str,
    revision_id: &str,
) -> Result<RevisionRow, HarnessError> {
    let query = format!(
        "select {REVISION_COLUMNS} \
         from harness_revisions r join harnesses h on h.harness_id = r.harness_id \
         where r.harness_id = $1 and r.harness_revision_id = $2"
    );
    sqlx::query_as::<_, RevisionRow>(&query)
        .bind(harness_id)
        .bind(revision_id)
        .fetch_one(&mut **transaction)
        .await
        .map_err(Into::into)
}
