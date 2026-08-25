use chrono::{DateTime, Utc};
use llm_contracts::{JsonObject, Message, Validate as _};
use serde_json::Value;
use sqlx::{FromRow, types::Json};
use uuid::Uuid;

use super::{ExecutionError, QueuedRunMessage, QueuedRunMessageState, Run, RunLease, RunStatus};
use crate::harnesses::{HarnessRevision, HarnessRevisionStatus};
use crate::sessions::{SessionMessage, SessionMessageDelivery, SessionMessageOrigin};

pub(super) const RUN_COLUMNS: &str = "run_id, session_id, trigger_message_id, \
    harness_revision_id, resolved_config, status, current_turn, max_turns, \
    failures_in_current_turn, max_failures_per_turn, state_version, queued_at, \
    final_message_id, failure, created_at, started_at, finished_at";

pub(super) const COMMITTED_MESSAGE_COLUMNS: &str = "session_message_id, session_id, revision, \
    message, origin, delivery, run_id, turn_number, created_at, committed_at";

pub(super) const QUEUED_MESSAGE_COLUMNS: &str = "session_message_id, session_id, message, state, \
    revision, run_id, queued_during_turn, queue_sequence, discard_reason, created_at, \
    committed_at, discarded_at";

#[derive(FromRow)]
pub(super) struct RunRow {
    pub run_id: Uuid,
    pub session_id: Uuid,
    pub trigger_message_id: Uuid,
    pub harness_revision_id: String,
    pub resolved_config: Json<JsonObject>,
    pub status: String,
    pub current_turn: i32,
    pub max_turns: i32,
    pub failures_in_current_turn: i32,
    pub max_failures_per_turn: i32,
    pub state_version: i64,
    pub queued_at: Option<DateTime<Utc>>,
    pub final_message_id: Option<Uuid>,
    pub failure: Option<Json<Value>>,
    pub created_at: DateTime<Utc>,
    pub started_at: Option<DateTime<Utc>>,
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
            failures_in_current_turn: non_negative_u32(
                "runs.failures_in_current_turn",
                row.failures_in_current_turn,
            )?,
            max_failures_per_turn: positive_u32(
                "runs.max_failures_per_turn",
                row.max_failures_per_turn,
            )?,
            state_version: positive_u64("runs.state_version", row.state_version)?,
            queued_at: row.queued_at,
            final_message_id: row.final_message_id,
            failure: row.failure.map(|value| value.0),
            created_at: row.created_at,
            started_at: row.started_at,
            finished_at: row.finished_at,
        })
    }
}

#[derive(FromRow)]
pub(super) struct CommittedMessageRow {
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
pub(super) struct SessionStateRow {
    pub current_revision: i64,
}

#[derive(FromRow)]
pub(super) struct RevisionConfigurationRow {
    pub harness_revision_id: String,
    pub harness_id: String,
    pub default_config: Json<JsonObject>,
    pub config_schema: Option<Json<JsonObject>>,
}

#[derive(FromRow)]
pub(super) struct LeaseRow {
    pub lease_id: Uuid,
    pub run_id: Uuid,
    pub lease_version: i64,
    pub worker_instance_id: String,
    pub token_hash: Vec<u8>,
    pub acquired_at: DateTime<Utc>,
    pub expires_at: DateTime<Utc>,
    pub active: bool,
}

impl TryFrom<&LeaseRow> for RunLease {
    type Error = ExecutionError;

    fn try_from(row: &LeaseRow) -> Result<Self, Self::Error> {
        Ok(Self {
            lease_id: row.lease_id,
            run_id: row.run_id,
            lease_version: positive_u64("run_leases.lease_version", row.lease_version)?,
            worker_instance_id: row.worker_instance_id.clone(),
            acquired_at: row.acquired_at,
            expires_at: row.expires_at,
        })
    }
}

#[derive(FromRow)]
pub(super) struct WorkerRevisionRow {
    pub harness_revision_id: String,
    pub harness_id: String,
    pub revision: String,
    pub contract_version: i64,
    pub default_config: Json<JsonObject>,
    pub config_schema: Option<Json<JsonObject>>,
    pub first_activated_at: Option<DateTime<Utc>>,
    pub retired_at: Option<DateTime<Utc>>,
    pub created_at: DateTime<Utc>,
    pub active_revision_id: Option<String>,
}

impl TryFrom<WorkerRevisionRow> for HarnessRevision {
    type Error = ExecutionError;

    fn try_from(row: WorkerRevisionRow) -> Result<Self, Self::Error> {
        let status = if row.retired_at.is_some() {
            HarnessRevisionStatus::Retired
        } else if row.active_revision_id.as_deref() == Some(&row.harness_revision_id) {
            HarnessRevisionStatus::Active
        } else if row.first_activated_at.is_some() {
            HarnessRevisionStatus::Deprecated
        } else {
            HarnessRevisionStatus::Registered
        };
        Ok(Self {
            harness_revision_id: row.harness_revision_id,
            harness_id: row.harness_id,
            revision: row.revision,
            contract_version: u32::try_from(row.contract_version)
                .map_err(|error| invalid("harness_revisions.contract_version", error))?,
            status,
            default_config: row.default_config.0,
            config_schema: row.config_schema.map(|value| value.0),
            first_activated_at: row.first_activated_at,
            retired_at: row.retired_at,
            created_at: row.created_at,
        })
    }
}

#[derive(FromRow)]
pub(super) struct QueuedMessageRow {
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
                .map(|value| positive_u64("session_messages.revision", value))
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

fn required<T>(field: &str, value: Option<T>) -> Result<T, ExecutionError> {
    value.ok_or_else(|| invalid(field, "is unexpectedly null"))
}

pub(super) fn positive_u32(field: &str, value: i32) -> Result<u32, ExecutionError> {
    let value = u32::try_from(value).map_err(|error| invalid(field, error))?;
    if value == 0 {
        return Err(invalid(field, "must be positive"));
    }
    Ok(value)
}

fn non_negative_u32(field: &str, value: i32) -> Result<u32, ExecutionError> {
    u32::try_from(value).map_err(|error| invalid(field, error))
}

pub(super) fn positive_u64(field: &str, value: i64) -> Result<u64, ExecutionError> {
    let value = u64::try_from(value).map_err(|error| invalid(field, error))?;
    if value == 0 {
        return Err(invalid(field, "must be positive"));
    }
    Ok(value)
}

fn invalid(field: &str, error: impl std::fmt::Display) -> ExecutionError {
    ExecutionError::InvalidStoredData(format!("{field}: {error}"))
}
