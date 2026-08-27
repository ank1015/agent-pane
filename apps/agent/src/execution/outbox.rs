use std::time::Duration;

use async_nats::{HeaderMap, jetstream};
use serde::Serialize;
use serde_json::Value;
use sqlx::{FromRow, Postgres, Transaction, types::Json};
use tokio::task::JoinHandle;
use tokio_util::sync::CancellationToken;
use uuid::Uuid;

use crate::db::Database;

use super::ExecutionError;

pub(super) async fn enqueue<T: Serialize>(
    transaction: &mut Transaction<'_, Postgres>,
    event_id: Uuid,
    subject: &str,
    payload: &T,
) -> Result<(), ExecutionError> {
    let payload = serde_json::to_value(payload).map_err(|error| {
        ExecutionError::InvalidStoredData(format!("could not serialize broker event: {error}"))
    })?;
    sqlx::query("insert into broker_outbox (event_id, subject, payload) values ($1, $2, $3)")
        .bind(event_id)
        .bind(subject)
        .bind(Json(payload))
        .execute(&mut **transaction)
        .await?;
    Ok(())
}

#[derive(FromRow)]
struct OutboxRow {
    event_id: Uuid,
    subject: String,
    payload: Json<Value>,
}

pub fn spawn_outbox_publisher(
    database: Database,
    jetstream: jetstream::Context,
    interval: Duration,
    batch_size: i64,
    shutdown: CancellationToken,
) -> JoinHandle<()> {
    tokio::spawn(async move {
        let mut ticker = tokio::time::interval(interval);
        ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
        loop {
            tokio::select! {
                () = shutdown.cancelled() => break,
                _ = ticker.tick() => {
                    if let Err(error) = publish_once(&database, &jetstream, batch_size).await {
                        tracing::warn!(%error, "could not publish Agent outbox batch");
                    }
                }
            }
        }
    })
}

async fn publish_once(
    database: &Database,
    jetstream: &jetstream::Context,
    batch_size: i64,
) -> Result<usize, Box<dyn std::error::Error + Send + Sync>> {
    let mut transaction = database.pool().begin().await?;
    let rows = sqlx::query_as::<_, OutboxRow>(
        "select event_id, subject, payload from broker_outbox \
         where published_at is null order by created_at, event_id \
         for update skip locked limit $1",
    )
    .bind(batch_size)
    .fetch_all(&mut *transaction)
    .await?;

    for row in &rows {
        let mut headers = HeaderMap::new();
        headers.insert("Nats-Msg-Id", row.event_id.to_string());
        let bytes = serde_json::to_vec(&row.payload.0)?;
        jetstream
            .publish_with_headers(row.subject.clone(), headers, bytes.into())
            .await?
            .await?;
        sqlx::query("update broker_outbox set published_at = now() where event_id = $1")
            .bind(row.event_id)
            .execute(&mut *transaction)
            .await?;
    }
    transaction.commit().await?;
    Ok(rows.len())
}
