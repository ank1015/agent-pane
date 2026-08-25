use sqlx::{Postgres, QueryBuilder};

use super::{
    CreateHarness, CreateOutcome, Harness, HarnessError, HarnessListQuery, Page, SetHarnessEnabled,
    UpdateHarness,
    cursor::{decode, encode},
    page_limit,
    records::{HARNESS_COLUMNS, HarnessRow},
};
use crate::db::Database;

pub(super) async fn create(
    database: &Database,
    command: CreateHarness,
) -> Result<CreateOutcome<Harness>, HarnessError> {
    command.validate()?;
    let query = format!(
        "insert into harnesses (harness_id, slug, display_name, description) \
         values ($1, $2, $3, $4) \
         on conflict (harness_id) do nothing \
         returning {HARNESS_COLUMNS}"
    );
    let inserted = sqlx::query_as::<_, HarnessRow>(&query)
        .bind(&command.harness_id)
        .bind(&command.slug)
        .bind(&command.display_name)
        .bind(&command.description)
        .fetch_optional(database.pool())
        .await;

    let inserted = match inserted {
        Ok(inserted) => inserted,
        Err(error) if super::constraint(&error) == Some("harnesses_slug_key") => {
            return Err(HarnessError::HarnessSlugConflict(command.slug));
        }
        Err(error) => return Err(error.into()),
    };
    if let Some(row) = inserted {
        return Ok(CreateOutcome {
            value: row.into(),
            created: true,
        });
    }

    let existing = get(database, &command.harness_id).await?;
    if existing.slug != command.slug {
        return Err(HarnessError::HarnessIdConflict(command.harness_id));
    }
    Ok(CreateOutcome {
        value: existing,
        created: false,
    })
}

pub(super) async fn list(
    database: &Database,
    query: HarnessListQuery,
) -> Result<Page<Harness>, HarnessError> {
    query.validate()?;
    let limit = page_limit(query.limit)?;
    let cursor = query.cursor.as_deref().map(decode).transpose()?;

    let mut sql = QueryBuilder::<Postgres>::new("select ");
    sql.push(HARNESS_COLUMNS).push(" from harnesses where true");
    if let Some(enabled) = query.enabled {
        sql.push(" and enabled = ").push_bind(enabled);
    }
    if let Some(slug) = query.slug {
        sql.push(" and slug = ").push_bind(slug);
    }
    if let Some(cursor) = cursor {
        sql.push(" and (created_at, harness_id) > (")
            .push_bind(cursor.created_at)
            .push(", ")
            .push_bind(cursor.id)
            .push(')');
    }
    sql.push(" order by created_at, harness_id limit ")
        .push_bind(i64::from(limit) + 1);

    let mut rows = sql
        .build_query_as::<HarnessRow>()
        .fetch_all(database.pool())
        .await?;
    let has_more = rows.len() > limit as usize;
    rows.truncate(limit as usize);
    let next_cursor = if has_more {
        rows.last()
            .map(|row| encode(row.created_at, &row.harness_id))
            .transpose()?
    } else {
        None
    };

    Ok(Page {
        items: rows.into_iter().map(Into::into).collect(),
        next_cursor,
    })
}

pub(super) async fn get(database: &Database, harness_id: &str) -> Result<Harness, HarnessError> {
    let query = format!("select {HARNESS_COLUMNS} from harnesses where harness_id = $1");
    sqlx::query_as::<_, HarnessRow>(&query)
        .bind(harness_id)
        .fetch_optional(database.pool())
        .await?
        .map(Into::into)
        .ok_or_else(|| HarnessError::HarnessNotFound(harness_id.to_owned()))
}

pub(super) async fn update(
    database: &Database,
    harness_id: &str,
    command: UpdateHarness,
) -> Result<Harness, HarnessError> {
    command.validate()?;
    let update_description = command.description.is_some();
    let description = command.description.flatten();
    let query = format!(
        "update harnesses \
         set display_name = coalesce($2, display_name), \
             description = case when $3 then $4 else description end \
         where harness_id = $1 \
         returning {HARNESS_COLUMNS}"
    );
    sqlx::query_as::<_, HarnessRow>(&query)
        .bind(harness_id)
        .bind(command.display_name)
        .bind(update_description)
        .bind(description)
        .fetch_optional(database.pool())
        .await?
        .map(Into::into)
        .ok_or_else(|| HarnessError::HarnessNotFound(harness_id.to_owned()))
}

pub(super) async fn set_enabled(
    database: &Database,
    harness_id: &str,
    command: SetHarnessEnabled,
) -> Result<Harness, HarnessError> {
    let mut transaction = database.pool().begin().await?;
    let current = sqlx::query_as::<_, HarnessRow>(&format!(
        "select {HARNESS_COLUMNS} from harnesses where harness_id = $1 for update"
    ))
    .bind(harness_id)
    .fetch_optional(&mut *transaction)
    .await?
    .ok_or_else(|| HarnessError::HarnessNotFound(harness_id.to_owned()))?;

    if command.enabled && current.active_revision_id.is_none() {
        return Err(HarnessError::NoActiveRevision);
    }
    let updated = sqlx::query_as::<_, HarnessRow>(&format!(
        "update harnesses set enabled = $2 where harness_id = $1 returning {HARNESS_COLUMNS}"
    ))
    .bind(harness_id)
    .bind(command.enabled)
    .fetch_one(&mut *transaction)
    .await?;
    transaction.commit().await?;
    Ok(updated.into())
}
