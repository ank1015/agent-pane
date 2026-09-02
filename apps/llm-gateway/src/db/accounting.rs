use base64::{Engine as _, engine::general_purpose::URL_SAFE_NO_PAD};
use chrono::{DateTime, Utc};
use llm_contracts::{AssistantMessage, ModelRef, Usage};
use serde::{Deserialize, Serialize};
use sqlx::{FromRow, types::Json};
use uuid::Uuid;

use super::Database;

pub const DEFAULT_COMPLETION_REQUEST_LIMIT: u32 = 25;
pub const MAX_COMPLETION_REQUEST_LIMIT: u32 = 100;

#[derive(Debug, Serialize)]
pub struct CompletionUsageSummary {
    pub account_id: Uuid,
    pub request_count: i64,
    pub costs: CompletionCostTotals,
    pub tokens: CompletionTokenTotals,
}

#[derive(Debug, Serialize)]
pub struct CompletionCostTotals {
    pub total: f64,
    pub input: f64,
    pub output: f64,
    pub cache_read: f64,
    pub cache_write: f64,
}

#[derive(Debug, Serialize)]
pub struct CompletionTokenTotals {
    pub input: i64,
    pub output: i64,
    pub cache_read: i64,
    pub cache_write: i64,
}

#[derive(Debug, Serialize)]
pub struct CompletionRequestPage {
    pub items: Vec<CompletionRequest>,
    pub next_cursor: Option<String>,
}

#[derive(Debug, Serialize)]
pub struct CompletionRequest {
    pub request_id: Uuid,
    pub requested_provider: String,
    pub requested_model: String,
    pub response_provider: String,
    pub response_model: String,
    pub assistant_message_id: String,
    pub usage: Option<Usage>,
    pub duration_ms: i64,
    pub completed_at: DateTime<Utc>,
}

#[derive(Debug, thiserror::Error)]
pub enum CompletionAccountingQueryError {
    #[error("cursor is invalid")]
    InvalidCursor,
    #[error("limit must be between 1 and {MAX_COMPLETION_REQUEST_LIMIT}")]
    InvalidLimit,
    #[error("completion accounting query failed")]
    Database(#[from] sqlx::Error),
}

impl Database {
    pub async fn record_completion_accounting(
        &self,
        request_id: Uuid,
        account_id: Uuid,
        requested_model: &ModelRef,
        message: &AssistantMessage,
    ) -> Result<(), sqlx::Error> {
        let usage = message.usage.clone().map(Json);

        sqlx::query(
            "insert into llm_completion_accounting (
                request_id,
                account_id,
                requested_provider,
                requested_model,
                response_provider,
                response_model,
                assistant_message_id,
                usage,
                provider_duration_ms
             ) values ($1, $2, $3, $4, $5, $6, $7, $8, $9::numeric)",
        )
        .bind(request_id)
        .bind(account_id)
        .bind(requested_model.provider.as_str())
        .bind(requested_model.id.as_str())
        .bind(message.model.provider.as_str())
        .bind(message.model.id.as_str())
        .bind(message.id.as_str())
        .bind(usage)
        .bind(message.duration_ms.to_string())
        .execute(&self.pool)
        .await?;

        Ok(())
    }

    pub async fn completion_usage_summary(
        &self,
        account_id: Uuid,
    ) -> Result<CompletionUsageSummary, sqlx::Error> {
        let totals = sqlx::query_as::<_, CompletionUsageSummaryRow>(
            "select
                count(*)::bigint as request_count,
                coalesce(sum(total_cost_usd), 0)::double precision as total_cost,
                coalesce(sum(input_cost_usd), 0)::double precision as input_cost,
                coalesce(sum(output_cost_usd), 0)::double precision as output_cost,
                coalesce(sum(cache_read_cost_usd), 0)::double precision as cache_read_cost,
                coalesce(sum(cache_write_cost_usd), 0)::double precision as cache_write_cost,
                coalesce(sum(input_tokens), 0)::bigint as input_tokens,
                coalesce(sum(output_tokens), 0)::bigint as output_tokens,
                coalesce(sum(cache_read_tokens), 0)::bigint as cache_read_tokens,
                coalesce(sum(cache_write_tokens), 0)::bigint as cache_write_tokens
             from llm_completion_accounting
             where account_id = $1",
        )
        .bind(account_id)
        .fetch_one(&self.pool)
        .await?;

        Ok(CompletionUsageSummary {
            account_id,
            request_count: totals.request_count,
            costs: CompletionCostTotals {
                total: totals.total_cost,
                input: totals.input_cost,
                output: totals.output_cost,
                cache_read: totals.cache_read_cost,
                cache_write: totals.cache_write_cost,
            },
            tokens: CompletionTokenTotals {
                input: totals.input_tokens,
                output: totals.output_tokens,
                cache_read: totals.cache_read_tokens,
                cache_write: totals.cache_write_tokens,
            },
        })
    }

    pub async fn recent_completion_requests(
        &self,
        account_id: Uuid,
        cursor: Option<&str>,
        limit: Option<u32>,
    ) -> Result<CompletionRequestPage, CompletionAccountingQueryError> {
        let limit = limit.unwrap_or(DEFAULT_COMPLETION_REQUEST_LIMIT);
        if !(1..=MAX_COMPLETION_REQUEST_LIMIT).contains(&limit) {
            return Err(CompletionAccountingQueryError::InvalidLimit);
        }
        let cursor = cursor.map(decode_cursor).transpose()?;
        let fetch_limit = i64::from(limit) + 1;

        let mut rows = match cursor {
            Some(cursor) => {
                sqlx::query_as::<_, CompletionRequestRow>(
                    "select
                        request_id,
                        requested_provider,
                        requested_model,
                        response_provider,
                        response_model,
                        assistant_message_id,
                        usage,
                        provider_duration_ms::bigint as duration_ms,
                        completed_at
                     from llm_completion_accounting
                     where account_id = $1
                       and (completed_at, request_id) < ($2, $3)
                     order by completed_at desc, request_id desc
                     limit $4",
                )
                .bind(account_id)
                .bind(cursor.completed_at)
                .bind(cursor.request_id)
                .bind(fetch_limit)
                .fetch_all(&self.pool)
                .await?
            }
            None => {
                sqlx::query_as::<_, CompletionRequestRow>(
                    "select
                        request_id,
                        requested_provider,
                        requested_model,
                        response_provider,
                        response_model,
                        assistant_message_id,
                        usage,
                        provider_duration_ms::bigint as duration_ms,
                        completed_at
                     from llm_completion_accounting
                     where account_id = $1
                     order by completed_at desc, request_id desc
                     limit $2",
                )
                .bind(account_id)
                .bind(fetch_limit)
                .fetch_all(&self.pool)
                .await?
            }
        };

        let has_more = rows.len() > limit as usize;
        rows.truncate(limit as usize);
        let next_cursor = if has_more {
            rows.last()
                .map(|row| encode_cursor(row.completed_at, row.request_id))
                .transpose()?
        } else {
            None
        };

        Ok(CompletionRequestPage {
            items: rows.into_iter().map(Into::into).collect(),
            next_cursor,
        })
    }
}

#[derive(FromRow)]
struct CompletionUsageSummaryRow {
    request_count: i64,
    total_cost: f64,
    input_cost: f64,
    output_cost: f64,
    cache_read_cost: f64,
    cache_write_cost: f64,
    input_tokens: i64,
    output_tokens: i64,
    cache_read_tokens: i64,
    cache_write_tokens: i64,
}

#[derive(FromRow)]
struct CompletionRequestRow {
    request_id: Uuid,
    requested_provider: String,
    requested_model: String,
    response_provider: String,
    response_model: String,
    assistant_message_id: String,
    usage: Option<Json<Usage>>,
    duration_ms: i64,
    completed_at: DateTime<Utc>,
}

impl From<CompletionRequestRow> for CompletionRequest {
    fn from(row: CompletionRequestRow) -> Self {
        Self {
            request_id: row.request_id,
            requested_provider: row.requested_provider,
            requested_model: row.requested_model,
            response_provider: row.response_provider,
            response_model: row.response_model,
            assistant_message_id: row.assistant_message_id,
            usage: row.usage.map(|usage| usage.0),
            duration_ms: row.duration_ms,
            completed_at: row.completed_at,
        }
    }
}

#[derive(Serialize, Deserialize)]
struct CompletionRequestCursor {
    completed_at_us: i64,
    request_id: Uuid,
}

struct DecodedCursor {
    completed_at: DateTime<Utc>,
    request_id: Uuid,
}

fn encode_cursor(
    completed_at: DateTime<Utc>,
    request_id: Uuid,
) -> Result<String, CompletionAccountingQueryError> {
    let bytes = serde_json::to_vec(&CompletionRequestCursor {
        completed_at_us: completed_at.timestamp_micros(),
        request_id,
    })
    .map_err(|_| CompletionAccountingQueryError::InvalidCursor)?;
    Ok(URL_SAFE_NO_PAD.encode(bytes))
}

fn decode_cursor(value: &str) -> Result<DecodedCursor, CompletionAccountingQueryError> {
    let bytes = URL_SAFE_NO_PAD
        .decode(value)
        .map_err(|_| CompletionAccountingQueryError::InvalidCursor)?;
    let cursor: CompletionRequestCursor = serde_json::from_slice(&bytes)
        .map_err(|_| CompletionAccountingQueryError::InvalidCursor)?;
    let completed_at = DateTime::from_timestamp_micros(cursor.completed_at_us)
        .ok_or(CompletionAccountingQueryError::InvalidCursor)?;
    Ok(DecodedCursor {
        completed_at,
        request_id: cursor.request_id,
    })
}

#[cfg(test)]
mod tests {
    use chrono::{TimeZone as _, Utc};
    use uuid::Uuid;

    use super::{decode_cursor, encode_cursor};

    #[test]
    fn cursor_preserves_postgres_timestamp_and_request_id() {
        let completed_at = Utc
            .with_ymd_and_hms(2026, 9, 2, 12, 30, 45)
            .single()
            .expect("timestamp")
            + chrono::Duration::microseconds(123_456);
        let request_id = Uuid::parse_str("01992aa0-0000-7000-8000-000000000001").unwrap();

        let decoded = decode_cursor(&encode_cursor(completed_at, request_id).unwrap()).unwrap();

        assert_eq!(decoded.completed_at, completed_at);
        assert_eq!(decoded.request_id, request_id);
    }

    #[test]
    fn cursor_rejects_invalid_values() {
        assert!(decode_cursor("not-a-cursor").is_err());
    }
}
