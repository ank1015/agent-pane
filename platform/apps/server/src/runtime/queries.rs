use super::{
    RuntimeService,
    error::{Result, RuntimeError},
    model::*,
};
use base64::{Engine, engine::general_purpose::URL_SAFE_NO_PAD};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sqlx::{PgConnection, Postgres, QueryBuilder};
use uuid::Uuid;

pub(super) const RUN_JSON: &str = "to_jsonb(r) - 'worker_id' - 'lease_epoch' - 'lease_expires_at'";

fn session_projection() -> String {
    format!(
        "to_jsonb(s) || jsonb_build_object('active_run', (select {RUN_JSON} from runs r where r.session_id=s.id and r.status in ('ready','running','waiting')))"
    )
}

pub(super) async fn session_json(db: &mut PgConnection, id: Uuid) -> Result<Value> {
    sqlx::query_scalar(&format!(
        "select {} from sessions s where s.id=$1",
        session_projection()
    ))
    .bind(id)
    .fetch_optional(db)
    .await?
    .ok_or(RuntimeError::NotFound)
}
pub(super) async fn run_json(db: &mut PgConnection, id: Uuid) -> Result<Value> {
    sqlx::query_scalar(&format!("select {RUN_JSON} from runs r where id=$1"))
        .bind(id)
        .fetch_optional(db)
        .await?
        .ok_or(RuntimeError::NotFound)
}

#[derive(Serialize, Deserialize)]
pub(super) struct Cursor {
    tag: String,
    pub at: DateTime<Utc>,
    pub id: Uuid,
}
pub(super) fn cursor(raw: Option<&str>, tag: &str) -> Result<Option<Cursor>> {
    let Some(raw) = raw else { return Ok(None) };
    let invalid = || RuntimeError::Invalid("Invalid cursor for this collection.");
    if raw.len() > 2048 {
        return Err(invalid());
    }
    let value: Cursor =
        serde_json::from_slice(&URL_SAFE_NO_PAD.decode(raw).map_err(|_| invalid())?)
            .map_err(|_| invalid())?;
    if value.tag != tag {
        return Err(invalid());
    }
    Ok(Some(value))
}
pub(super) fn page(mut items: Vec<Value>, count: i64, tag: String, column: &str) -> Result<Value> {
    let more = items.len() > count as usize;
    items.truncate(count as usize);
    let next = if more {
        let item = items.last().ok_or(RuntimeError::StoredData)?;
        let c = Cursor {
            tag,
            at: serde_json::from_value(item[column].clone())
                .map_err(|_| RuntimeError::StoredData)?,
            id: serde_json::from_value(item["id"].clone()).map_err(|_| RuntimeError::StoredData)?,
        };
        Some(URL_SAFE_NO_PAD.encode(serde_json::to_vec(&c).map_err(|_| RuntimeError::StoredData)?))
    } else {
        None
    };
    Ok(json!({"items":items,"next_cursor":next}))
}
pub(super) fn sequence_page(
    mut items: Vec<Value>,
    count: i64,
    field: &str,
    next_field: &str,
) -> Value {
    let more = items.len() > count as usize;
    items.truncate(count as usize);
    let next = if more {
        items.last().map(|v| v[field].clone())
    } else {
        None
    };
    json!({"items":items,next_field:next})
}

impl RuntimeService {
    pub(super) async fn harnesses(&self) -> Result<Value> {
        let items: Vec<Value> = sqlx::query_scalar(
            "select to_jsonb(h) from harnesses h where enabled order by name,id",
        )
        .fetch_all(&self.pool)
        .await?;
        Ok(json!({"items":items}))
    }
    pub(super) async fn harness(&self, id: &str) -> Result<Value> {
        sqlx::query_scalar("select to_jsonb(h) from harnesses h where id=$1")
            .bind(id)
            .fetch_optional(&self.pool)
            .await?
            .ok_or(RuntimeError::NotFound)
    }
    pub(super) async fn session(&self, id: Uuid) -> Result<Value> {
        session_json(&mut *self.pool.acquire().await?, id).await
    }
    pub(super) async fn run(&self, id: Uuid) -> Result<Value> {
        run_json(&mut *self.pool.acquire().await?, id).await
    }
    pub(super) async fn exists(
        &self,
        table: &'static str,
        column: &'static str,
        id: Uuid,
    ) -> Result<()> {
        let found: bool = sqlx::query_scalar(&format!(
            "select exists(select 1 from {table} where {column}=$1)"
        ))
        .bind(id)
        .fetch_one(&self.pool)
        .await?;
        if found {
            Ok(())
        } else {
            Err(RuntimeError::NotFound)
        }
    }
    pub(super) async fn sessions(&self, project: Uuid, q: ListQuery) -> Result<Value> {
        if q.status.is_some() {
            return Err(RuntimeError::Invalid(
                "Sessions do not support status filtering.",
            ));
        }
        self.exists("projects", "project_id", project).await?;
        let count = limit(q.limit)?;
        let archived = q.archived.unwrap_or(false);
        let tag = format!("sessions:v1:{project}:{archived}");
        let c = cursor(q.cursor.as_deref(), &tag)?;
        let mut query = QueryBuilder::<Postgres>::new(format!(
            "select {} from sessions s where project_id=",
            session_projection()
        ));
        query
            .push_bind(project)
            .push(" and (archived_at is not null)=")
            .push_bind(archived);
        if let Some(c) = c {
            query
                .push(" and (last_activity_at,id)<(")
                .push_bind(c.at)
                .push(",")
                .push_bind(c.id)
                .push(")");
        }
        query
            .push(" order by last_activity_at desc,id desc limit ")
            .push_bind(count + 1);
        page(
            query.build_query_scalar().fetch_all(&self.pool).await?,
            count,
            tag,
            "last_activity_at",
        )
    }
    pub(super) async fn runs(&self, id: Uuid, children: bool, q: ListQuery) -> Result<Value> {
        if q.archived.is_some() {
            return Err(RuntimeError::Invalid(
                "Runs do not support archived filtering.",
            ));
        }
        if let Some(status) = &q.status {
            if ![
                "ready",
                "running",
                "waiting",
                "completed",
                "failed",
                "aborted",
            ]
            .contains(&status.as_str())
            {
                return Err(RuntimeError::Invalid("Invalid run status."));
            }
        }
        self.exists(if children { "runs" } else { "sessions" }, "id", id)
            .await?;
        let count = limit(q.limit)?;
        let tag = format!("runs:v1:{id}:{children}:{:?}", q.status);
        let c = cursor(q.cursor.as_deref(), &tag)?;
        let mut query = QueryBuilder::<Postgres>::new(format!(
            "select {RUN_JSON} from runs r where {}=",
            if children {
                "parent_run_id"
            } else {
                "session_id"
            }
        ));
        query.push_bind(id);
        if let Some(status) = q.status {
            query.push(" and status=").push_bind(status);
        }
        if let Some(c) = c {
            query
                .push(" and (created_at,id)>(")
                .push_bind(c.at)
                .push(",")
                .push_bind(c.id)
                .push(")");
        }
        query
            .push(" order by created_at,id limit ")
            .push_bind(count + 1);
        page(
            query.build_query_scalar().fetch_all(&self.pool).await?,
            count,
            tag,
            "created_at",
        )
    }
    pub(super) async fn messages(&self, id: Uuid, q: MessageQuery) -> Result<Value> {
        self.exists("sessions", "id", id).await?;
        let count = limit(q.limit)?;
        let after = after(q.after_revision)?;
        if let Some(run) = q.run_id {
            let found: bool = sqlx::query_scalar(
                "select exists(select 1 from runs where id=$1 and session_id=$2)",
            )
            .bind(run)
            .bind(id)
            .fetch_one(&self.pool)
            .await?;
            if !found {
                return Err(RuntimeError::NotFound);
            }
        }
        let items=sqlx::query_scalar("select jsonb_build_object('message_id',m.id,'revision',sm.revision,'run_id',sm.run_id,'origin_run_id',m.origin_run_id,'message',m.message,'created_at',m.created_at) from session_messages sm join messages m on m.id=sm.message_id where sm.session_id=$1 and sm.revision>$2 and ($3::uuid is null or sm.run_id=$3) order by sm.revision limit $4")
            .bind(id).bind(after).bind(q.run_id).bind(count+1).fetch_all(&self.pool).await?;
        Ok(sequence_page(
            items,
            count,
            "revision",
            "next_after_revision",
        ))
    }
    pub(super) async fn inputs(&self, id: Uuid, q: SequenceQuery) -> Result<Value> {
        self.exists("runs", "id", id).await?;
        if let Some(status) = &q.status {
            if !["pending", "handled", "rejected"].contains(&status.as_str()) {
                return Err(RuntimeError::Invalid("Invalid input status."));
            }
        }
        let count = limit(q.limit)?;
        let items=sqlx::query_scalar("select to_jsonb(i) - 'deduplication_key' from run_inputs i where run_id=$1 and sequence>$2 and ($3::text is null or status=$3) order by sequence limit $4")
            .bind(id).bind(after(q.after_sequence)?).bind(q.status).bind(count+1).fetch_all(&self.pool).await?;
        Ok(sequence_page(
            items,
            count,
            "sequence",
            "next_after_sequence",
        ))
    }
    pub(super) async fn event_batch(&self, id: Uuid, after: i64, count: i64) -> Result<Vec<Value>> {
        Ok(sqlx::query_scalar("select to_jsonb(e) from run_events e where run_id=$1 and sequence>$2 order by sequence limit $3").bind(id).bind(after).bind(count).fetch_all(&self.pool).await?)
    }
    pub(super) async fn events(&self, id: Uuid, q: SequenceQuery) -> Result<Value> {
        if q.status.is_some() {
            return Err(RuntimeError::Invalid(
                "Events do not support status filtering.",
            ));
        }
        self.exists("runs", "id", id).await?;
        let count = limit(q.limit)?;
        Ok(sequence_page(
            self.event_batch(id, after(q.after_sequence)?, count + 1)
                .await?,
            count,
            "sequence",
            "next_after_sequence",
        ))
    }
    pub(super) async fn waits(&self, id: Uuid, q: ListQuery) -> Result<Value> {
        self.exists("runs", "id", id).await?;
        if q.archived.is_some() {
            return Err(RuntimeError::Invalid(
                "Waits do not support archived filtering.",
            ));
        }
        if let Some(status) = &q.status {
            if !["pending", "satisfied", "cancelled", "timed_out"].contains(&status.as_str()) {
                return Err(RuntimeError::Invalid("Invalid wait status."));
            }
        }
        let count = limit(q.limit)?;
        let tag = format!("waits:v1:{id}:{:?}", q.status);
        let c = cursor(q.cursor.as_deref(), &tag)?;
        let mut query = QueryBuilder::<Postgres>::new(
            "select to_jsonb(w) || jsonb_build_object('dependencies',coalesce((select jsonb_agg(to_jsonb(d) order by d.id) from run_wait_dependencies d where d.wait_id=w.id),'[]'::jsonb)) from run_waits w where run_id=",
        );
        query.push_bind(id);
        if let Some(status) = q.status {
            query.push(" and status=").push_bind(status);
        }
        if let Some(c) = c {
            query
                .push(" and (created_at,id)>(")
                .push_bind(c.at)
                .push(",")
                .push_bind(c.id)
                .push(")");
        }
        query
            .push(" order by created_at,id limit ")
            .push_bind(count + 1);
        page(
            query.build_query_scalar().fetch_all(&self.pool).await?,
            count,
            tag,
            "created_at",
        )
    }
}
