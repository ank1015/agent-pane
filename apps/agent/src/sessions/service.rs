use sqlx::QueryBuilder;
use uuid::Uuid;

use super::{
    CreateOutcome, CreateSession, Session, SessionError, SessionMessage, SessionMessageListQuery,
    SessionMessagePage, SessionRunListQuery, SessionRunPage, SessionRunSummary, cursor,
    message_page_limit,
    records::{
        MESSAGE_COLUMNS, RUN_SUMMARY_COLUMNS, SESSION_COLUMNS, SessionMessageRow, SessionRow,
        SessionRunSummaryRow,
    },
    run_page_limit,
};
use crate::db::Database;

pub(super) async fn create(
    database: &Database,
    request: CreateSession,
) -> Result<CreateOutcome<Session>, SessionError> {
    let query = format!(
        "INSERT INTO sessions (session_id) VALUES ($1) \
         ON CONFLICT (session_id) DO NOTHING RETURNING {SESSION_COLUMNS}"
    );
    let inserted = sqlx::query_as::<_, SessionRow>(&query)
        .bind(request.session_id)
        .fetch_optional(database.pool())
        .await?;

    match inserted {
        Some(row) => Ok(CreateOutcome {
            value: row.try_into()?,
            created: true,
        }),
        None => Ok(CreateOutcome {
            value: get(database, request.session_id).await?,
            created: false,
        }),
    }
}

pub(super) async fn get(database: &Database, session_id: Uuid) -> Result<Session, SessionError> {
    let query = format!("SELECT {SESSION_COLUMNS} FROM sessions WHERE session_id = $1");
    let row = sqlx::query_as::<_, SessionRow>(&query)
        .bind(session_id)
        .fetch_optional(database.pool())
        .await?
        .ok_or(SessionError::NotFound(session_id))?;
    row.try_into()
}

pub(super) async fn list_messages(
    database: &Database,
    session_id: Uuid,
    query: SessionMessageListQuery,
) -> Result<SessionMessagePage, SessionError> {
    get(database, session_id).await?;
    let limit = message_page_limit(query.limit)?;
    let after_revision = i64::try_from(query.after_revision.unwrap_or(0))
        .map_err(|_| SessionError::InvalidAfterRevision)?;
    let fetch_limit = i64::from(limit) + 1;
    let statement = format!(
        "SELECT {MESSAGE_COLUMNS} FROM session_messages \
         WHERE session_id = $1 AND state = 'committed' AND revision > $2 \
         ORDER BY revision ASC LIMIT $3"
    );
    let rows = sqlx::query_as::<_, SessionMessageRow>(&statement)
        .bind(session_id)
        .bind(after_revision)
        .bind(fetch_limit)
        .fetch_all(database.pool())
        .await?;

    let has_more = rows.len() > limit as usize;
    let items = rows
        .into_iter()
        .take(limit as usize)
        .map(SessionMessage::try_from)
        .collect::<Result<Vec<_>, _>>()?;
    let next_after_revision = if has_more {
        items.last().map(|item| item.revision)
    } else {
        None
    };

    Ok(SessionMessagePage {
        items,
        next_after_revision,
    })
}

pub(super) async fn list_runs(
    database: &Database,
    session_id: Uuid,
    query: SessionRunListQuery,
) -> Result<SessionRunPage, SessionError> {
    get(database, session_id).await?;
    let limit = run_page_limit(query.limit)?;
    let decoded_cursor = query.cursor.as_deref().map(cursor::decode).transpose()?;

    let mut statement = QueryBuilder::new(format!(
        "SELECT {RUN_SUMMARY_COLUMNS} FROM runs WHERE session_id = "
    ));
    statement.push_bind(session_id);
    if let Some(status) = query.status {
        statement.push(" AND status = ").push_bind(status.as_db());
    }
    if let Some(cursor) = decoded_cursor {
        statement
            .push(" AND (created_at, run_id) > (")
            .push_bind(cursor.created_at)
            .push(", ")
            .push_bind(cursor.run_id)
            .push(")");
    }
    statement
        .push(" ORDER BY created_at ASC, run_id ASC LIMIT ")
        .push_bind(i64::from(limit) + 1);

    let rows = statement
        .build_query_as::<SessionRunSummaryRow>()
        .fetch_all(database.pool())
        .await?;
    let has_more = rows.len() > limit as usize;
    let items = rows
        .into_iter()
        .take(limit as usize)
        .map(SessionRunSummary::try_from)
        .collect::<Result<Vec<_>, _>>()?;
    let next_cursor = if has_more {
        items
            .last()
            .map(|run| cursor::encode(run.created_at, run.run_id))
            .transpose()?
    } else {
        None
    };

    Ok(SessionRunPage { items, next_cursor })
}
