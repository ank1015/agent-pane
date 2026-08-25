use chrono::{DateTime, Utc};
use llm_contracts::JsonObject;
use sqlx::{FromRow, types::Json};
use uuid::Uuid;

use super::{
    AbortFinalizationReason, AbortResume, ExecutionError, RunAbort, RunAbortStatus,
    records::{positive_u32, positive_u64},
};

pub(super) const ABORT_COLUMNS: &str = "abort_id, run_id, turn_number, sequence, reason, payload, \
    status, requested_at, delivered_at, deadline_at, finalized_at, finalization_reason, \
    resume_metadata, resolution, resumed_at, resumed_turn_number";

#[derive(FromRow)]
pub(super) struct AbortRow {
    pub abort_id: Uuid,
    pub run_id: Uuid,
    pub turn_number: i32,
    pub sequence: i64,
    pub reason: Option<String>,
    pub payload: Json<JsonObject>,
    pub status: String,
    pub requested_at: DateTime<Utc>,
    pub delivered_at: Option<DateTime<Utc>>,
    pub deadline_at: DateTime<Utc>,
    pub finalized_at: Option<DateTime<Utc>>,
    pub finalization_reason: Option<String>,
    pub resume_metadata: Json<JsonObject>,
    pub resolution: Option<Json<JsonObject>>,
    pub resumed_at: Option<DateTime<Utc>>,
    pub resumed_turn_number: Option<i32>,
}

impl TryFrom<AbortRow> for RunAbort {
    type Error = ExecutionError;

    fn try_from(row: AbortRow) -> Result<Self, Self::Error> {
        Ok(Self {
            abort_id: row.abort_id,
            run_id: row.run_id,
            turn_number: positive_u32("run_aborts.turn_number", row.turn_number)?,
            sequence: positive_u64("run_aborts.sequence", row.sequence)?,
            reason: row.reason,
            payload: row.payload.0,
            status: RunAbortStatus::from_db(&row.status)
                .ok_or_else(|| invalid("run_aborts.status", &row.status))?,
            requested_at: row.requested_at,
            delivered_at: row.delivered_at,
            deadline_at: row.deadline_at,
            finalized_at: row.finalized_at,
            finalization_reason: row
                .finalization_reason
                .map(|value| {
                    AbortFinalizationReason::from_db(&value)
                        .ok_or_else(|| invalid("run_aborts.finalization_reason", value))
                })
                .transpose()?,
            resume_metadata: row.resume_metadata.0,
            resolution: row.resolution.map(|value| value.0),
            resumed_at: row.resumed_at,
            resumed_turn_number: row
                .resumed_turn_number
                .map(|value| positive_u32("run_aborts.resumed_turn_number", value))
                .transpose()?,
        })
    }
}

impl AbortRow {
    pub(super) fn resume(self) -> Result<AbortResume, ExecutionError> {
        Ok(AbortResume {
            abort_id: self.abort_id,
            reason: self.reason,
            payload: self.payload.0,
            resume_metadata: self.resume_metadata.0,
            resolution: self
                .resolution
                .ok_or_else(|| invalid("run_aborts.resolution", "is unexpectedly null"))?
                .0,
            finalized_at: self
                .finalized_at
                .ok_or_else(|| invalid("run_aborts.finalized_at", "is unexpectedly null"))?,
        })
    }
}

fn invalid(field: &str, error: impl std::fmt::Display) -> ExecutionError {
    ExecutionError::InvalidStoredData(format!("{field}: {error}"))
}
