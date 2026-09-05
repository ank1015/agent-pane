use chrono::{DateTime, Utc};
use llm_contracts::{JsonObject, Message, Validate};
use serde::{Deserialize, Deserializer, Serialize};
use serde_json::{Value, json};
use uuid::Uuid;

use super::error::{Result, RuntimeError};

pub use platform_runtime_contracts::StartRun;
pub(super) use platform_runtime_contracts::{ListQuery, MessageQuery, SequenceQuery};

pub(super) trait ValidateStartRun {
    fn validate(&self) -> Result<()>;
}
impl ValidateStartRun for StartRun {
    fn validate(&self) -> Result<()> {
        validate_user_message(&self.input)?;
        if self.expected_session_revision < 0 {
            return Err(RuntimeError::Invalid(
                "expected_session_revision must be nonnegative.",
            ));
        }
        Ok(())
    }
}

#[derive(Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct CreateSession {
    pub harness_id: String,
    pub title: Option<String>,
    #[serde(default)]
    pub config_override: JsonObject,
    pub initial_run: Option<StartRun>,
}

#[derive(Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ForkSession {
    pub at_revision: i64,
    pub harness_id: Option<String>,
    pub title: Option<String>,
    #[serde(default)]
    pub config_override: JsonObject,
    pub initial_run: Option<StartRun>,
}

#[derive(Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct PatchSession {
    #[serde(
        default,
        deserialize_with = "present_title",
        skip_serializing_if = "Option::is_none"
    )]
    pub title: Option<Option<String>>,
    #[serde(default, deserialize_with = "present_bool")]
    pub archived: Option<bool>,
}

fn present_title<'de, D: Deserializer<'de>>(
    d: D,
) -> std::result::Result<Option<Option<String>>, D::Error> {
    Option::<String>::deserialize(d).map(Some)
}
fn present_bool<'de, D: Deserializer<'de>>(d: D) -> std::result::Result<Option<bool>, D::Error> {
    bool::deserialize(d).map(Some)
}

#[derive(Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields, tag = "kind", rename_all = "snake_case")]
pub enum SubmitInput {
    UserMessage {
        message: Box<Message>,
    },
    ApprovalResponse {
        correlation_key: String,
        approved: bool,
        #[serde(default)]
        data: JsonObject,
    },
}

impl SubmitInput {
    pub fn parts(&self) -> Result<(&'static str, Value)> {
        match self {
            Self::UserMessage { message } => {
                validate_user_message(message)?;
                Ok(("user_message", json!({"message":message})))
            }
            Self::ApprovalResponse {
                correlation_key,
                approved,
                data,
            } => {
                nonempty(
                    correlation_key,
                    256,
                    "Provide a nonempty correlation_key of at most 256 characters.",
                )?;
                Ok((
                    "approval_response",
                    json!({"correlation_key":correlation_key,"approved":approved,"data":data}),
                ))
            }
        }
    }
}

#[derive(Debug, Default, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct AbortRun {
    pub reason: Option<String>,
}

pub(super) fn validate_user_message(message: &Message) -> Result<()> {
    if !matches!(message, Message::User(_)) || message.validate().is_err() {
        return Err(RuntimeError::Invalid(
            "Provide a valid user message using the common message contract.",
        ));
    }
    Ok(())
}

pub(super) fn nonempty(value: &str, max: usize, message: &'static str) -> Result<()> {
    if value.trim().is_empty() || value != value.trim() || value.chars().count() > max {
        return Err(RuntimeError::Invalid(message));
    }
    Ok(())
}

pub(super) fn validate_title(value: Option<&str>) -> Result<()> {
    if let Some(value) = value {
        nonempty(
            value,
            512,
            "Titles must contain 1–512 characters without surrounding whitespace.",
        )?;
    }
    Ok(())
}

pub(super) fn limit(value: Option<u32>) -> Result<i64> {
    match value.unwrap_or(50) {
        n @ 1..=200 => Ok(i64::from(n)),
        _ => Err(RuntimeError::Invalid("limit must be between 1 and 200.")),
    }
}

pub(super) fn after(value: Option<i64>) -> Result<i64> {
    match value.unwrap_or(0) {
        n if n >= 0 => Ok(n),
        _ => Err(RuntimeError::Invalid(
            "Sequence and revision cursors must be nonnegative.",
        )),
    }
}

#[derive(sqlx::FromRow)]
pub(super) struct SessionRow {
    pub id: Uuid,
    pub project_id: Uuid,
    pub harness_id: String,
    pub config: Value,
    pub title: Option<String>,
    pub current_revision: i64,
    pub archived_at: Option<DateTime<Utc>>,
}

#[derive(sqlx::FromRow)]
pub(super) struct RunRow {
    pub id: Uuid,
    pub project_id: Uuid,
    pub session_id: Uuid,
    pub status: String,
    pub abort_requested_at: Option<DateTime<Utc>>,
}

impl RunRow {
    pub fn live(&self) -> Result<()> {
        if matches!(self.status.as_str(), "completed" | "failed" | "aborted") {
            return Err(RuntimeError::CodedConflict(
                platform_runtime_contracts::ConflictCode::RunStateConflict,
                "The run is terminal and cannot accept new input.",
            ));
        }
        Ok(())
    }
}
