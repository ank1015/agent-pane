use chrono::{DateTime, Utc};
use llm_contracts::{Message, Validate as _};
use serde_json::Value;
use sqlx::{FromRow, types::Json};
use uuid::Uuid;

use super::{
    RunStatus, Session, SessionError, SessionMessage, SessionMessageDelivery, SessionMessageOrigin,
    SessionRunSummary,
};

pub(super) const SESSION_COLUMNS: &str = "session_id, current_revision, created_at";

pub(super) const MESSAGE_COLUMNS: &str = "session_message_id, session_id, revision, message, \
    origin, delivery, run_id, turn_number, created_at, committed_at";

pub(super) const RUN_SUMMARY_COLUMNS: &str = "run_id, session_id, trigger_message_id, \
    harness_revision_id, status, current_turn, max_turns, state_version, final_message_id, \
    failure, created_at, activated_at, finished_at";

#[derive(FromRow)]
pub(super) struct SessionRow {
    session_id: Uuid,
    current_revision: i64,
    created_at: DateTime<Utc>,
}

impl TryFrom<SessionRow> for Session {
    type Error = SessionError;

    fn try_from(row: SessionRow) -> Result<Self, Self::Error> {
        Ok(Self {
            session_id: row.session_id,
            current_revision: non_negative_u64("sessions.current_revision", row.current_revision)?,
            created_at: row.created_at,
        })
    }
}

#[derive(FromRow)]
pub(super) struct SessionMessageRow {
    session_message_id: Uuid,
    session_id: Uuid,
    revision: Option<i64>,
    message: Json<Message>,
    origin: String,
    delivery: String,
    run_id: Option<Uuid>,
    turn_number: Option<i32>,
    created_at: DateTime<Utc>,
    committed_at: Option<DateTime<Utc>>,
}

impl TryFrom<SessionMessageRow> for SessionMessage {
    type Error = SessionError;

    fn try_from(row: SessionMessageRow) -> Result<Self, Self::Error> {
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
pub(super) struct SessionRunSummaryRow {
    run_id: Uuid,
    session_id: Uuid,
    trigger_message_id: Uuid,
    harness_revision_id: String,
    status: String,
    current_turn: i32,
    max_turns: i32,
    state_version: i64,
    final_message_id: Option<Uuid>,
    failure: Option<Json<Value>>,
    created_at: DateTime<Utc>,
    activated_at: DateTime<Utc>,
    finished_at: Option<DateTime<Utc>>,
}

impl TryFrom<SessionRunSummaryRow> for SessionRunSummary {
    type Error = SessionError;

    fn try_from(row: SessionRunSummaryRow) -> Result<Self, Self::Error> {
        Ok(Self {
            run_id: row.run_id,
            session_id: row.session_id,
            trigger_message_id: row.trigger_message_id,
            harness_revision_id: row.harness_revision_id,
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

fn required<T>(field: &str, value: Option<T>) -> Result<T, SessionError> {
    value.ok_or_else(|| invalid(field, "is unexpectedly null"))
}

fn positive_u32(field: &str, value: i32) -> Result<u32, SessionError> {
    let value = u32::try_from(value).map_err(|error| invalid(field, error))?;
    if value == 0 {
        return Err(invalid(field, "must be positive"));
    }
    Ok(value)
}

fn positive_u64(field: &str, value: i64) -> Result<u64, SessionError> {
    let value = u64::try_from(value).map_err(|error| invalid(field, error))?;
    if value == 0 {
        return Err(invalid(field, "must be positive"));
    }
    Ok(value)
}

fn non_negative_u64(field: &str, value: i64) -> Result<u64, SessionError> {
    u64::try_from(value).map_err(|error| invalid(field, error))
}

fn invalid(field: &str, error: impl std::fmt::Display) -> SessionError {
    SessionError::InvalidStoredData(format!("{field}: {error}"))
}
