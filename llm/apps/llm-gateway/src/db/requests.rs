use std::{fmt, str::FromStr};

use base64::{Engine as _, engine::general_purpose::URL_SAFE_NO_PAD};
use chrono::{DateTime, Utc};
use llm_contracts::Usage;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use sqlx::{FromRow, Postgres, QueryBuilder, types::Json};
use uuid::Uuid;

use super::Database;

pub const DEFAULT_REQUEST_LIMIT: u32 = 25;
pub const MAX_REQUEST_LIMIT: u32 = 100;

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum LlmOperation {
    Complete,
    Search,
}

impl LlmOperation {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Complete => "complete",
            Self::Search => "search",
        }
    }
}

impl fmt::Display for LlmOperation {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.as_str())
    }
}

impl FromStr for LlmOperation {
    type Err = RequestQueryError;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        match value {
            "complete" => Ok(Self::Complete),
            "search" => Ok(Self::Search),
            _ => Err(RequestQueryError::InvalidOperation),
        }
    }
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq)]
#[serde(rename_all = "snake_case")]
pub enum UsageGroupBy {
    Account,
    Provider,
    Model,
    Day,
}

#[derive(Clone, Debug, Default)]
pub struct LlmRequestFilters {
    pub account_id: Option<Uuid>,
    pub provider: Option<String>,
    pub model: Option<String>,
    pub operation: Option<LlmOperation>,
    pub status: Option<String>,
    pub from: Option<DateTime<Utc>>,
    pub to: Option<DateTime<Utc>>,
}

#[derive(Debug, Serialize)]
pub struct LlmRequestPage {
    pub items: Vec<LlmRequestRecord>,
    pub next_cursor: Option<String>,
}

#[derive(Debug, Serialize)]
pub struct LlmRequestRecord {
    pub id: Uuid,
    pub operation: String,
    pub account_id: Uuid,
    pub provider: String,
    pub requested_model: String,
    pub response_provider: Option<String>,
    pub response_model: Option<String>,
    pub response_id: Option<String>,
    pub status: String,
    pub error_kind: Option<String>,
    pub provider_error_code: Option<String>,
    pub can_retry: Option<bool>,
    pub labels: Value,
    pub usage: Option<Usage>,
    pub provider_duration_ms: Option<i64>,
    pub gateway_duration_ms: Option<i64>,
    pub started_at: DateTime<Utc>,
    pub completed_at: Option<DateTime<Utc>>,
}

#[derive(Debug, Serialize)]
pub struct UsageReport {
    pub totals: UsageTotals,
    pub groups: Vec<UsageGroup>,
}

#[derive(Debug, Serialize)]
pub struct UsageGroup {
    pub key: String,
    #[serde(flatten)]
    pub totals: UsageTotals,
}

#[derive(Debug, Serialize)]
pub struct UsageTotals {
    pub request_count: i64,
    pub succeeded_count: i64,
    pub failed_count: i64,
    pub usage_record_count: i64,
    pub tokens: TokenTotals,
    pub costs: CostTotals,
}

#[derive(Debug, Serialize)]
pub struct TokenTotals {
    pub input: i64,
    pub output: i64,
    pub cache_read: i64,
    pub cache_write: i64,
}

#[derive(Debug, Serialize)]
pub struct CostTotals {
    pub total: f64,
    pub input: f64,
    pub output: f64,
    pub cache_read: f64,
    pub cache_write: f64,
}

impl Database {
    pub async fn begin_llm_request(
        &self,
        id: Uuid,
        operation: LlmOperation,
        account_id: Uuid,
        provider: &str,
        requested_model: &str,
        labels: &Value,
    ) -> Result<(), sqlx::Error> {
        sqlx::query(
            "insert into llm_requests (
                id, operation, account_id, provider, requested_model, labels
             ) values ($1, $2, $3, $4, $5, $6)",
        )
        .bind(id)
        .bind(operation.as_str())
        .bind(account_id)
        .bind(provider)
        .bind(requested_model)
        .bind(labels)
        .execute(self.pool())
        .await?;
        Ok(())
    }

    #[allow(clippy::too_many_arguments)]
    pub async fn finish_llm_request_success(
        &self,
        request_id: Uuid,
        response_provider: Option<&str>,
        response_model: Option<&str>,
        response_id: Option<&str>,
        provider_duration_ms: Option<u64>,
        gateway_duration_ms: u64,
        usage: Option<&Usage>,
    ) -> Result<(), sqlx::Error> {
        let mut transaction = self.pool().begin().await?;
        sqlx::query(
            "update llm_requests
             set response_provider = $2, response_model = $3, response_id = $4,
                 status = 'succeeded', provider_duration_ms = $5,
                 gateway_duration_ms = $6, completed_at = now()
             where id = $1 and status = 'running'",
        )
        .bind(request_id)
        .bind(response_provider)
        .bind(response_model)
        .bind(response_id)
        .bind(provider_duration_ms.map(saturating_milliseconds))
        .bind(saturating_milliseconds(gateway_duration_ms))
        .execute(&mut *transaction)
        .await?;
        if let Some(usage) = usage {
            sqlx::query("insert into llm_usage (request_id, usage) values ($1, $2)")
                .bind(request_id)
                .bind(Json(usage))
                .execute(&mut *transaction)
                .await?;
        }
        transaction.commit().await?;
        Ok(())
    }

    pub async fn finish_llm_request_failure(
        &self,
        request_id: Uuid,
        status: &str,
        error_kind: &str,
        provider_error_code: Option<&str>,
        can_retry: bool,
        gateway_duration_ms: u64,
    ) -> Result<(), sqlx::Error> {
        sqlx::query(
            "update llm_requests
             set status = $2, error_kind = $3, provider_error_code = $4,
                 can_retry = $5, gateway_duration_ms = $6, completed_at = now()
             where id = $1 and status = 'running'",
        )
        .bind(request_id)
        .bind(status)
        .bind(error_kind)
        .bind(provider_error_code)
        .bind(can_retry)
        .bind(saturating_milliseconds(gateway_duration_ms))
        .execute(self.pool())
        .await?;
        Ok(())
    }

    pub async fn find_llm_request(
        &self,
        request_id: Uuid,
    ) -> Result<Option<LlmRequestRecord>, sqlx::Error> {
        sqlx::query_as::<_, LlmRequestRow>(&format!("{} where r.id = $1", request_select()))
            .bind(request_id)
            .fetch_optional(self.pool())
            .await
            .map(|row| row.map(Into::into))
    }

    pub async fn recent_llm_requests(
        &self,
        filters: &LlmRequestFilters,
        cursor: Option<&str>,
        limit: Option<u32>,
    ) -> Result<LlmRequestPage, RequestQueryError> {
        let limit = limit.unwrap_or(DEFAULT_REQUEST_LIMIT);
        if !(1..=MAX_REQUEST_LIMIT).contains(&limit) {
            return Err(RequestQueryError::InvalidLimit);
        }
        let cursor = cursor.map(decode_cursor).transpose()?;
        let mut query = QueryBuilder::<Postgres>::new(request_select());
        query.push(" where true");
        push_filters(&mut query, filters);
        if let Some(cursor) = cursor {
            query
                .push(" and (r.started_at, r.id) < (")
                .push_bind(cursor.started_at)
                .push(", ")
                .push_bind(cursor.request_id)
                .push(")");
        }
        query
            .push(" order by r.started_at desc, r.id desc limit ")
            .push_bind(i64::from(limit) + 1);
        let mut rows = query
            .build_query_as::<LlmRequestRow>()
            .fetch_all(self.pool())
            .await?;
        let has_more = rows.len() > limit as usize;
        rows.truncate(limit as usize);
        let next_cursor = if has_more {
            rows.last()
                .map(|row| encode_cursor(row.started_at, row.id))
                .transpose()?
        } else {
            None
        };
        Ok(LlmRequestPage {
            items: rows.into_iter().map(Into::into).collect(),
            next_cursor,
        })
    }

    pub async fn usage_report(
        &self,
        filters: &LlmRequestFilters,
        group_by: Option<UsageGroupBy>,
    ) -> Result<UsageReport, sqlx::Error> {
        let totals = aggregate_query(self.pool(), filters, None).await?;
        let groups = match group_by {
            Some(group_by) => aggregate_query(self.pool(), filters, Some(group_by))
                .await?
                .into_iter()
                .map(|mut row| UsageGroup {
                    key: row.group_key.take().unwrap_or_default(),
                    totals: row.into(),
                })
                .collect(),
            None => Vec::new(),
        };
        Ok(UsageReport {
            totals: totals.into_iter().next().unwrap_or_default().into(),
            groups,
        })
    }
}

fn request_select() -> &'static str {
    "select r.id, r.operation, r.account_id, r.provider, r.requested_model,
            r.response_provider, r.response_model, r.response_id, r.status,
            r.error_kind, r.provider_error_code, r.can_retry, r.labels,
            u.usage, r.provider_duration_ms, r.gateway_duration_ms,
            r.started_at, r.completed_at
     from llm_requests r left join llm_usage u on u.request_id = r.id"
}

fn push_filters<'a>(query: &mut QueryBuilder<'a, Postgres>, filters: &'a LlmRequestFilters) {
    if let Some(account_id) = filters.account_id {
        query.push(" and r.account_id = ").push_bind(account_id);
    }
    if let Some(provider) = &filters.provider {
        query.push(" and r.provider = ").push_bind(provider);
    }
    if let Some(model) = &filters.model {
        query.push(" and r.requested_model = ").push_bind(model);
    }
    if let Some(operation) = filters.operation {
        query
            .push(" and r.operation = ")
            .push_bind(operation.as_str());
    }
    if let Some(status) = &filters.status {
        query.push(" and r.status = ").push_bind(status);
    }
    if let Some(from) = filters.from {
        query.push(" and r.started_at >= ").push_bind(from);
    }
    if let Some(to) = filters.to {
        query.push(" and r.started_at < ").push_bind(to);
    }
}

async fn aggregate_query(
    pool: &sqlx::PgPool,
    filters: &LlmRequestFilters,
    group_by: Option<UsageGroupBy>,
) -> Result<Vec<UsageAggregateRow>, sqlx::Error> {
    let group_expression = group_by.map(|group| match group {
        UsageGroupBy::Account => "r.account_id::text",
        UsageGroupBy::Provider => "r.provider",
        UsageGroupBy::Model => "r.requested_model",
        UsageGroupBy::Day => {
            "to_char(date_trunc('day', r.started_at at time zone 'UTC'), 'YYYY-MM-DD')"
        }
    });
    let mut query = QueryBuilder::<Postgres>::new("select ");
    if let Some(expression) = group_expression {
        query.push(expression).push(" as group_key, ");
    } else {
        query.push("null::text as group_key, ");
    }
    query.push(
        "count(*)::bigint as request_count,
         count(*) filter (where r.status = 'succeeded')::bigint as succeeded_count,
         count(*) filter (where r.status not in ('running', 'succeeded'))::bigint as failed_count,
         count(u.request_id)::bigint as usage_record_count,
         coalesce(sum(u.input_tokens), 0)::bigint as input_tokens,
         coalesce(sum(u.output_tokens), 0)::bigint as output_tokens,
         coalesce(sum(u.cache_read_tokens), 0)::bigint as cache_read_tokens,
         coalesce(sum(u.cache_write_tokens), 0)::bigint as cache_write_tokens,
         coalesce(sum(u.input_cost_usd), 0)::double precision as input_cost,
         coalesce(sum(u.output_cost_usd), 0)::double precision as output_cost,
         coalesce(sum(u.cache_read_cost_usd), 0)::double precision as cache_read_cost,
         coalesce(sum(u.cache_write_cost_usd), 0)::double precision as cache_write_cost,
         coalesce(sum(u.total_cost_usd), 0)::double precision as total_cost
         from llm_requests r left join llm_usage u on u.request_id = r.id where true",
    );
    push_filters(&mut query, filters);
    if let Some(expression) = group_expression {
        query
            .push(" group by ")
            .push(expression)
            .push(" order by ")
            .push(expression);
    }
    query
        .build_query_as::<UsageAggregateRow>()
        .fetch_all(pool)
        .await
}

fn saturating_milliseconds(value: u64) -> i64 {
    i64::try_from(value).unwrap_or(i64::MAX)
}

#[derive(FromRow)]
struct LlmRequestRow {
    id: Uuid,
    operation: String,
    account_id: Uuid,
    provider: String,
    requested_model: String,
    response_provider: Option<String>,
    response_model: Option<String>,
    response_id: Option<String>,
    status: String,
    error_kind: Option<String>,
    provider_error_code: Option<String>,
    can_retry: Option<bool>,
    labels: Json<Value>,
    usage: Option<Json<Usage>>,
    provider_duration_ms: Option<i64>,
    gateway_duration_ms: Option<i64>,
    started_at: DateTime<Utc>,
    completed_at: Option<DateTime<Utc>>,
}

impl From<LlmRequestRow> for LlmRequestRecord {
    fn from(row: LlmRequestRow) -> Self {
        Self {
            id: row.id,
            operation: row.operation,
            account_id: row.account_id,
            provider: row.provider,
            requested_model: row.requested_model,
            response_provider: row.response_provider,
            response_model: row.response_model,
            response_id: row.response_id,
            status: row.status,
            error_kind: row.error_kind,
            provider_error_code: row.provider_error_code,
            can_retry: row.can_retry,
            labels: row.labels.0,
            usage: row.usage.map(|usage| usage.0),
            provider_duration_ms: row.provider_duration_ms,
            gateway_duration_ms: row.gateway_duration_ms,
            started_at: row.started_at,
            completed_at: row.completed_at,
        }
    }
}

#[derive(Default, FromRow)]
struct UsageAggregateRow {
    group_key: Option<String>,
    request_count: i64,
    succeeded_count: i64,
    failed_count: i64,
    usage_record_count: i64,
    input_tokens: i64,
    output_tokens: i64,
    cache_read_tokens: i64,
    cache_write_tokens: i64,
    input_cost: f64,
    output_cost: f64,
    cache_read_cost: f64,
    cache_write_cost: f64,
    total_cost: f64,
}

impl From<UsageAggregateRow> for UsageTotals {
    fn from(row: UsageAggregateRow) -> Self {
        Self {
            request_count: row.request_count,
            succeeded_count: row.succeeded_count,
            failed_count: row.failed_count,
            usage_record_count: row.usage_record_count,
            tokens: TokenTotals {
                input: row.input_tokens,
                output: row.output_tokens,
                cache_read: row.cache_read_tokens,
                cache_write: row.cache_write_tokens,
            },
            costs: CostTotals {
                total: row.total_cost,
                input: row.input_cost,
                output: row.output_cost,
                cache_read: row.cache_read_cost,
                cache_write: row.cache_write_cost,
            },
        }
    }
}

#[derive(Serialize, Deserialize)]
struct RequestCursor {
    started_at_us: i64,
    request_id: Uuid,
}

struct DecodedCursor {
    started_at: DateTime<Utc>,
    request_id: Uuid,
}

fn encode_cursor(started_at: DateTime<Utc>, request_id: Uuid) -> Result<String, RequestQueryError> {
    let bytes = serde_json::to_vec(&RequestCursor {
        started_at_us: started_at.timestamp_micros(),
        request_id,
    })
    .map_err(|_| RequestQueryError::InvalidCursor)?;
    Ok(URL_SAFE_NO_PAD.encode(bytes))
}

fn decode_cursor(value: &str) -> Result<DecodedCursor, RequestQueryError> {
    let bytes = URL_SAFE_NO_PAD
        .decode(value)
        .map_err(|_| RequestQueryError::InvalidCursor)?;
    let cursor: RequestCursor =
        serde_json::from_slice(&bytes).map_err(|_| RequestQueryError::InvalidCursor)?;
    let started_at = DateTime::from_timestamp_micros(cursor.started_at_us)
        .ok_or(RequestQueryError::InvalidCursor)?;
    Ok(DecodedCursor {
        started_at,
        request_id: cursor.request_id,
    })
}

#[derive(Debug, thiserror::Error)]
pub enum RequestQueryError {
    #[error("cursor is invalid")]
    InvalidCursor,
    #[error("limit must be between 1 and {MAX_REQUEST_LIMIT}")]
    InvalidLimit,
    #[error("operation must be complete or search")]
    InvalidOperation,
    #[error("request accounting query failed")]
    Database(#[from] sqlx::Error),
}

#[cfg(test)]
mod tests {
    use chrono::{TimeZone as _, Utc};
    use uuid::Uuid;

    use super::{decode_cursor, encode_cursor};

    #[test]
    fn cursor_preserves_timestamp_and_request_id() {
        let started_at = Utc
            .with_ymd_and_hms(2026, 9, 3, 12, 30, 45)
            .single()
            .unwrap()
            + chrono::Duration::microseconds(123_456);
        let request_id = Uuid::parse_str("01992aa0-0000-7000-8000-000000000001").unwrap();
        let decoded = decode_cursor(&encode_cursor(started_at, request_id).unwrap()).unwrap();
        assert_eq!(decoded.started_at, started_at);
        assert_eq!(decoded.request_id, request_id);
    }

    #[test]
    fn cursor_rejects_invalid_values() {
        assert!(decode_cursor("not-a-cursor").is_err());
    }
}
