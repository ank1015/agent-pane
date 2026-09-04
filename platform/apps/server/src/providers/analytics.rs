//! Allowlisted accounting DTOs. Never forward request labels, prompts or raw errors.
use super::error::ProviderError;
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

#[derive(Deserialize)]
pub(super) struct GatewayUsageReport {
    pub totals: UsageTotals,
}

#[derive(Deserialize, Serialize)]
pub struct UsageTotals {
    pub request_count: u64,
    pub succeeded_count: u64,
    pub failed_count: u64,
    pub usage_record_count: u64,
    pub tokens: TokenTotals,
    pub costs: CostTotals,
}

#[derive(Serialize)]
pub struct ProviderUsage {
    pub account_id: Uuid,
    #[serde(flatten)]
    pub totals: UsageTotals,
}

#[derive(Deserialize, Serialize)]
pub struct TokenTotals {
    pub input: u64,
    pub output: u64,
    pub cache_read: u64,
    pub cache_write: u64,
}

#[derive(Deserialize, Serialize)]
pub struct CostTotals {
    pub total: f64,
    pub input: f64,
    pub output: f64,
    pub cache_read: f64,
    pub cache_write: f64,
}

#[derive(Deserialize, Serialize)]
pub struct RequestUsage {
    pub input: Option<u64>,
    pub output: Option<u64>,
    pub cache_read: Option<u64>,
    pub cache_write: Option<u64>,
    pub cost: Option<RequestCost>,
}

#[derive(Deserialize, Serialize)]
pub struct RequestCost {
    pub total: f64,
    pub input: Option<f64>,
    pub output: Option<f64>,
    pub cache_read: Option<f64>,
    pub cache_write: Option<f64>,
}

#[derive(Deserialize, Serialize)]
pub struct ProviderRequest {
    pub id: Uuid,
    pub account_id: Uuid,
    pub provider: String,
    pub requested_model: String,
    pub response_model: Option<String>,
    pub status: String,
    pub usage: Option<RequestUsage>,
    pub started_at: DateTime<Utc>,
    pub completed_at: Option<DateTime<Utc>>,
}

#[derive(Deserialize, Serialize)]
pub struct ProviderRequestPage {
    pub items: Vec<ProviderRequest>,
    pub next_cursor: Option<String>,
}

#[derive(Default, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct RequestsQuery {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cursor: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub limit: Option<u32>,
}

impl RequestsQuery {
    pub fn validate(&self) -> Result<(), ProviderError> {
        if self.limit.is_some_and(|limit| !(1..=100).contains(&limit)) {
            return Err(ProviderError::InvalidRequest(
                "Limit must be between 1 and 100.",
            ));
        }
        if self.cursor.as_ref().is_some_and(|cursor| {
            cursor.is_empty() || cursor.len() > 2048 || cursor.chars().any(char::is_control)
        }) {
            return Err(ProviderError::InvalidRequest(
                "Invalid request-history cursor.",
            ));
        }
        Ok(())
    }
}
