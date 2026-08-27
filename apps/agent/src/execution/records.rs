use chrono::{DateTime, Utc};
use llm_contracts::{JsonObject, Message, Validate as _};
use serde_json::Value;
use sqlx::{FromRow, types::Json};
use uuid::Uuid;

use super::{
    ExecutionError, QueuedRunMessage, QueuedRunMessageState, Run, RunAbort, RunStatus, RunWait,
    WaitResolutionSource, WaitStatus,
};
use crate::sessions::{SessionMessage, SessionMessageDelivery, SessionMessageOrigin};

pub(super) const RUN_COLUMNS: &str = "run_id, session_id, trigger_message_id, harness_revision_id, resolved_config, status, current_turn, max_turns, state_version, final_message_id, failure, created_at, activated_at, finished_at";
pub(super) const COMMITTED_MESSAGE_COLUMNS: &str = "session_message_id, session_id, revision, message, origin, delivery, run_id, turn_number, created_at, committed_at";
pub(super) const QUEUED_MESSAGE_COLUMNS: &str = "session_message_id, session_id, message, state, revision, run_id, queued_during_turn, queue_sequence, discard_reason, created_at, committed_at, discarded_at";

#[derive(FromRow)]
pub(crate) struct RunRow {
    pub run_id: Uuid,
    pub session_id: Uuid,
    pub trigger_message_id: Uuid,
    pub harness_revision_id: String,
    pub resolved_config: Json<JsonObject>,
    pub status: String,
    pub current_turn: i32,
    pub max_turns: i32,
    pub state_version: i64,
    pub final_message_id: Option<Uuid>,
    pub failure: Option<Json<Value>>,
    pub created_at: DateTime<Utc>,
    pub activated_at: DateTime<Utc>,
    pub finished_at: Option<DateTime<Utc>>,
}

impl TryFrom<RunRow> for Run {
    type Error = ExecutionError;
    fn try_from(row: RunRow) -> Result<Self, Self::Error> {
        Ok(Self {
            run_id: row.run_id,
            session_id: row.session_id,
            trigger_message_id: row.trigger_message_id,
            harness_revision_id: row.harness_revision_id,
            resolved_config: row.resolved_config.0,
            status: RunStatus::from_db(&row.status)
                .ok_or_else(|| invalid("runs.status", &row.status))?,
            current_turn: positive_u32("runs.current_turn", row.current_turn)?,
            max_turns: positive_u32("runs.max_turns", row.max_turns)?,
            state_version: positive_u64("runs.state_version", row.state_version)?,
            final_message_id: row.final_message_id,
            failure: row.failure.map(|value| value.0),
            created_at: row.created_at,
            activated_at: row.activated_at,
            finished_at: row.finished_at,
        })
    }
}

#[derive(FromRow)]
pub(crate) struct RunContextRow {
    pub run_id: Uuid,
    pub session_id: Uuid,
    pub harness_revision_id: String,
    pub harness_id: String,
    pub harness_slug: String,
    pub resolved_config: Json<JsonObject>,
    pub status: String,
    pub current_turn: i32,
    pub max_turns: i32,
    pub state_version: i64,
    pub current_session_revision: i64,
}

#[derive(FromRow)]
pub(crate) struct RevisionConfigurationRow {
    pub harness_revision_id: String,
    pub default_config: Json<JsonObject>,
    pub config_schema: Option<Json<JsonObject>>,
}

#[derive(FromRow)]
pub(crate) struct CommittedMessageRow {
    pub session_message_id: Uuid,
    pub session_id: Uuid,
    pub revision: Option<i64>,
    pub message: Json<Message>,
    pub origin: String,
    pub delivery: String,
    pub run_id: Option<Uuid>,
    pub turn_number: Option<i32>,
    pub created_at: DateTime<Utc>,
    pub committed_at: Option<DateTime<Utc>>,
}

impl TryFrom<CommittedMessageRow> for SessionMessage {
    type Error = ExecutionError;
    fn try_from(row: CommittedMessageRow) -> Result<Self, Self::Error> {
        row.message
            .0
            .validate()
            .map_err(|error| invalid("session_messages.message", error))?;
        Ok(Self {
            session_message_id: row.session_message_id,
            session_id: row.session_id,
            revision: positive_u64(
                "session_messages.revision",
                required("session_messages.revision", row.revision)?,
            )?,
            message: row.message.0,
            origin: SessionMessageOrigin::from_db(&row.origin)
                .ok_or_else(|| invalid("session_messages.origin", &row.origin))?,
            delivery: SessionMessageDelivery::from_db(&row.delivery)
                .ok_or_else(|| invalid("session_messages.delivery", &row.delivery))?,
            run_id: row.run_id,
            turn_number: row
                .turn_number
                .map(|value| positive_u32("session_messages.turn_number", value))
                .transpose()?,
            created_at: row.created_at,
            committed_at: required("session_messages.committed_at", row.committed_at)?,
        })
    }
}

#[derive(FromRow)]
pub(crate) struct QueuedMessageRow {
    pub session_message_id: Uuid,
    pub session_id: Uuid,
    pub message: Json<Message>,
    pub state: String,
    pub revision: Option<i64>,
    pub run_id: Option<Uuid>,
    pub queued_during_turn: Option<i32>,
    pub queue_sequence: Option<i64>,
    pub discard_reason: Option<String>,
    pub created_at: DateTime<Utc>,
    pub committed_at: Option<DateTime<Utc>>,
    pub discarded_at: Option<DateTime<Utc>>,
}

impl TryFrom<QueuedMessageRow> for QueuedRunMessage {
    type Error = ExecutionError;
    fn try_from(row: QueuedMessageRow) -> Result<Self, Self::Error> {
        row.message
            .0
            .validate()
            .map_err(|error| invalid("session_messages.message", error))?;
        let state = match row.state.as_str() {
            "pending" => QueuedRunMessageState::Pending,
            "committed" => QueuedRunMessageState::Committed,
            "discarded" => QueuedRunMessageState::Discarded,
            other => return Err(invalid("session_messages.state", other)),
        };
        Ok(Self {
            session_message_id: row.session_message_id,
            session_id: row.session_id,
            message: row.message.0,
            state,
            revision: row
                .revision
                .map(|v| positive_u64("session_messages.revision", v))
                .transpose()?,
            run_id: required("session_messages.run_id", row.run_id)?,
            queued_during_turn: positive_u32(
                "session_messages.queued_during_turn",
                required(
                    "session_messages.queued_during_turn",
                    row.queued_during_turn,
                )?,
            )?,
            queue_sequence: positive_u64(
                "session_messages.queue_sequence",
                required("session_messages.queue_sequence", row.queue_sequence)?,
            )?,
            discard_reason: row.discard_reason,
            created_at: row.created_at,
            committed_at: row.committed_at,
            discarded_at: row.discarded_at,
        })
    }
}

#[derive(FromRow)]
pub(crate) struct WaitRow {
    pub wait_id: Uuid,
    pub run_id: Uuid,
    pub turn_number: i32,
    pub harness_wait_id: String,
    pub kind: String,
    pub public_request: Json<JsonObject>,
    pub resume_metadata: Json<JsonObject>,
    pub expires_at: Option<DateTime<Utc>>,
    pub status: String,
    pub resolution_source: Option<String>,
    pub resolution: Option<Json<JsonObject>>,
    pub requested_at: DateTime<Utc>,
    pub resolved_at: Option<DateTime<Utc>>,
    pub cancelled_at: Option<DateTime<Utc>>,
}

impl TryFrom<WaitRow> for RunWait {
    type Error = ExecutionError;
    fn try_from(row: WaitRow) -> Result<Self, Self::Error> {
        Ok(Self {
            wait_id: row.wait_id,
            run_id: row.run_id,
            turn_number: positive_u32("run_waits.turn_number", row.turn_number)?,
            harness_wait_id: row.harness_wait_id,
            kind: row.kind,
            public_request: row.public_request.0,
            resume_metadata: row.resume_metadata.0,
            expires_at: row.expires_at,
            status: WaitStatus::from_db(&row.status)
                .ok_or_else(|| invalid("run_waits.status", &row.status))?,
            resolution_source: row
                .resolution_source
                .map(|v| {
                    WaitResolutionSource::from_db(&v)
                        .ok_or_else(|| invalid("run_waits.resolution_source", &v))
                })
                .transpose()?,
            resolution: row.resolution.map(|v| v.0),
            requested_at: row.requested_at,
            resolved_at: row.resolved_at,
            cancelled_at: row.cancelled_at,
        })
    }
}

#[derive(FromRow)]
pub(crate) struct AbortRow {
    pub abort_id: Uuid,
    pub run_id: Uuid,
    pub turn_number: i32,
    pub reason: Option<String>,
    pub payload: Json<JsonObject>,
    pub requested_at: DateTime<Utc>,
}

impl TryFrom<AbortRow> for RunAbort {
    type Error = ExecutionError;
    fn try_from(row: AbortRow) -> Result<Self, Self::Error> {
        Ok(Self {
            abort_id: row.abort_id,
            run_id: row.run_id,
            turn_number: positive_u32("run_aborts.turn_number", row.turn_number)?,
            reason: row.reason,
            payload: row.payload.0,
            requested_at: row.requested_at,
        })
    }
}

pub(super) fn positive_u32(field: &str, value: i32) -> Result<u32, ExecutionError> {
    let value = u32::try_from(value).map_err(|e| invalid(field, e))?;
    if value == 0 {
        Err(invalid(field, "must be positive"))
    } else {
        Ok(value)
    }
}
pub(super) fn positive_u64(field: &str, value: i64) -> Result<u64, ExecutionError> {
    let value = u64::try_from(value).map_err(|e| invalid(field, e))?;
    if value == 0 {
        Err(invalid(field, "must be positive"))
    } else {
        Ok(value)
    }
}
pub(super) fn required<T>(field: &str, value: Option<T>) -> Result<T, ExecutionError> {
    value.ok_or_else(|| invalid(field, "is unexpectedly null"))
}
pub(super) fn invalid(field: &str, error: impl std::fmt::Display) -> ExecutionError {
    ExecutionError::InvalidStoredData(format!("{field}: {error}"))
}
