//! Durable Agent ↔ harness runtime contracts.
//!
//! Lifecycle traffic is carried through a durable, at-least-once broker. Agent
//! remains authoritative for run state and uses command IDs plus run state
//! versions to make duplicate and stale command handling deterministic. The
//! broker delivery itself is not represented as a lease or execution attempt.
//!
//! Session transcript reads and appends remain an HTTP API. Those request and
//! response types live here because they are the only runtime interaction that
//! is intentionally not transported through the broker.

use std::collections::HashSet;

use chrono::{DateTime, Utc};
use llm_contracts::{JsonObject, Message, Validate, ValidationError};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::{
    NewRunMessage, RunStatus, SessionMessage, WaitResume, validation::append_nested,
    validation::finish, validation::issue, validation::validate_id, validation::validate_trimmed,
};

/// Current major version of the Agent ↔ harness broker protocol.
pub const HARNESS_PROTOCOL_VERSION: u32 = 1;
pub const WORK_STREAM_NAME: &str = "AGENT_HARNESS_WORK";
pub const COMMAND_STREAM_NAME: &str = "AGENT_COMMANDS";
pub const EVENT_STREAM_NAME: &str = "AGENT_HARNESS_EVENTS";
pub const AGENT_COMMAND_CONSUMER_NAME: &str = "agent-commands-v1";
pub const WORK_SUBJECT_PATTERN: &str = "agent.harness.*.turn.requested.v1";
pub const COMMAND_SUBJECT_PATTERN: &str = "agent.run.*.command.v1";
pub const RESULT_SUBJECT_PATTERN: &str = "agent.harness.*.result.v1";
pub const CANCELLED_SUBJECT_PATTERN: &str = "agent.harness.*.run.cancelled.v1";

#[must_use]
pub fn turn_subject(harness_slug: &str) -> String {
    format!("agent.harness.{harness_slug}.turn.requested.v1")
}

#[must_use]
pub fn command_subject(run_id: Uuid) -> String {
    format!("agent.run.{run_id}.command.v1")
}

#[must_use]
pub fn result_subject(harness_slug: &str) -> String {
    format!("agent.harness.{harness_slug}.result.v1")
}

#[must_use]
pub fn cancelled_subject(harness_slug: &str) -> String {
    format!("agent.harness.{harness_slug}.run.cancelled.v1")
}

/// Durable notification that a run turn is eligible for harness processing.
#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct TurnRequested {
    pub protocol_version: u32,
    pub event_id: Uuid,
    pub emitted_at: DateTime<Utc>,
    pub run_id: Uuid,
    pub session_id: Uuid,
    pub turn_number: u32,
    pub max_turns: u32,
    pub expected_state_version: u64,
    pub harness_id: String,
    pub harness_slug: String,
    pub harness_revision_id: String,
    pub resolved_config: JsonObject,
    pub current_session_revision: u64,
    pub resume: Option<WaitResume>,
}

impl Validate for TurnRequested {
    fn validate(&self) -> Result<(), ValidationError> {
        let mut issues = Vec::new();
        validate_protocol_version(&mut issues, self.protocol_version);
        validate_uuid(&mut issues, "event_id", self.event_id);
        validate_uuid(&mut issues, "run_id", self.run_id);
        validate_uuid(&mut issues, "session_id", self.session_id);
        validate_positive(&mut issues, "turn_number", u64::from(self.turn_number));
        validate_positive(&mut issues, "max_turns", u64::from(self.max_turns));
        if self.turn_number > self.max_turns {
            issue(&mut issues, "turn_number", "must not exceed max_turns");
        }
        validate_positive(
            &mut issues,
            "expected_state_version",
            self.expected_state_version,
        );
        validate_id(&mut issues, "harness_id", &self.harness_id);
        validate_slug(&mut issues, "harness_slug", &self.harness_slug);
        validate_id(
            &mut issues,
            "harness_revision_id",
            &self.harness_revision_id,
        );
        if let Some(resume) = &self.resume
            && let Err(error) = resume.validate()
        {
            append_nested(&mut issues, "resume", error);
        }
        finish(issues)
    }
}

/// Best-effort cancellation notification for an active broker delivery.
///
/// Agent makes the durable abort decision before publishing this event. A late
/// harness outcome is rejected by its expected state version even if this
/// notification is delayed or missed; only the separately flagged and tightly
/// correlated tool-result cleanup append is accepted afterward.
#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct RunCancelled {
    pub protocol_version: u32,
    pub event_id: Uuid,
    pub emitted_at: DateTime<Utc>,
    pub run_id: Uuid,
    pub turn_number: u32,
    pub state_version: u64,
    pub reason: Option<String>,
    #[serde(default)]
    pub payload: JsonObject,
}

impl Validate for RunCancelled {
    fn validate(&self) -> Result<(), ValidationError> {
        let mut issues = Vec::new();
        validate_protocol_version(&mut issues, self.protocol_version);
        validate_uuid(&mut issues, "event_id", self.event_id);
        validate_uuid(&mut issues, "run_id", self.run_id);
        validate_positive(&mut issues, "turn_number", u64::from(self.turn_number));
        validate_positive(&mut issues, "state_version", self.state_version);
        if let Some(reason) = &self.reason {
            validate_trimmed(&mut issues, "reason", reason, 1, 2_000);
        }
        finish(issues)
    }
}

/// Idempotent lifecycle command published by a harness runtime.
#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct HarnessCommand {
    pub protocol_version: u32,
    pub command_id: Uuid,
    pub issued_at: DateTime<Utc>,
    pub run_id: Uuid,
    /// Slug of the harness publishing the command. Agent uses it to route the
    /// durable command result, including rejections for unknown run IDs.
    pub harness_slug: String,
    pub turn_number: u32,
    pub expected_state_version: u64,
    pub operation: HarnessOperation,
}

impl Validate for HarnessCommand {
    fn validate(&self) -> Result<(), ValidationError> {
        let mut issues = Vec::new();
        validate_protocol_version(&mut issues, self.protocol_version);
        validate_uuid(&mut issues, "command_id", self.command_id);
        validate_uuid(&mut issues, "run_id", self.run_id);
        validate_slug(&mut issues, "harness_slug", &self.harness_slug);
        validate_positive(&mut issues, "turn_number", u64::from(self.turn_number));
        validate_positive(
            &mut issues,
            "expected_state_version",
            self.expected_state_version,
        );
        if let Err(error) = self.operation.validate_at(self.issued_at) {
            append_nested(&mut issues, "operation", error);
        }
        finish(issues)
    }
}

/// The four state-changing decisions owned by a harness runtime.
#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
#[serde(tag = "type", content = "details", rename_all = "snake_case")]
pub enum HarnessOperation {
    Complete { final_message_id: Uuid },
    Continue,
    Fail { failure: JsonObject },
    Wait(WaitRequest),
}

impl HarnessOperation {
    fn validate_at(&self, issued_at: DateTime<Utc>) -> Result<(), ValidationError> {
        let mut issues = Vec::new();
        match self {
            Self::Complete { final_message_id } => {
                validate_uuid(&mut issues, "final_message_id", *final_message_id);
            }
            Self::Wait(wait) => {
                if let Err(error) = wait.validate_at(issued_at) {
                    append_nested(&mut issues, "details", error);
                }
            }
            Self::Continue | Self::Fail { .. } => {}
        }
        finish(issues)
    }
}

/// Harness-owned request to suspend a run until external resolution or expiry.
#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct WaitRequest {
    pub wait_id: Uuid,
    pub harness_wait_id: String,
    pub kind: String,
    #[serde(default)]
    pub public_request: JsonObject,
    #[serde(default)]
    pub resume_metadata: JsonObject,
    pub expires_at: Option<DateTime<Utc>>,
}

impl WaitRequest {
    fn validate_at(&self, issued_at: DateTime<Utc>) -> Result<(), ValidationError> {
        let mut issues = Vec::new();
        validate_uuid(&mut issues, "wait_id", self.wait_id);
        validate_trimmed(
            &mut issues,
            "harness_wait_id",
            &self.harness_wait_id,
            1,
            256,
        );
        validate_trimmed(&mut issues, "kind", &self.kind, 1, 128);
        if self
            .expires_at
            .is_some_and(|expires_at| expires_at <= issued_at)
        {
            issue(&mut issues, "expires_at", "must be later than issued_at");
        }
        finish(issues)
    }
}

/// Durable result emitted by Agent after consuming a harness command.
#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct HarnessCommandResult {
    pub protocol_version: u32,
    pub result_id: Uuid,
    pub command_id: Uuid,
    pub emitted_at: DateTime<Utc>,
    pub run_id: Uuid,
    pub outcome: HarnessCommandOutcome,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
#[serde(tag = "status", content = "details", rename_all = "snake_case")]
pub enum HarnessCommandOutcome {
    Applied(AppliedHarnessCommand),
    Duplicate(AppliedHarnessCommand),
    Rejected(RejectedHarnessCommand),
}

impl Validate for HarnessCommandResult {
    fn validate(&self) -> Result<(), ValidationError> {
        let mut issues = Vec::new();
        validate_protocol_version(&mut issues, self.protocol_version);
        validate_uuid(&mut issues, "result_id", self.result_id);
        validate_uuid(&mut issues, "command_id", self.command_id);
        validate_uuid(&mut issues, "run_id", self.run_id);
        match &self.outcome {
            HarnessCommandOutcome::Applied(applied) | HarnessCommandOutcome::Duplicate(applied) => {
                validate_positive(
                    &mut issues,
                    "outcome.details.run_state_version",
                    applied.run_state_version,
                );
            }
            HarnessCommandOutcome::Rejected(rejected) => {
                validate_trimmed(&mut issues, "outcome.details.code", &rejected.code, 1, 128);
                validate_trimmed(
                    &mut issues,
                    "outcome.details.message",
                    &rejected.message,
                    1,
                    2_000,
                );
                if rejected.actual_state_version == Some(0) {
                    issue(
                        &mut issues,
                        "outcome.details.actual_state_version",
                        "must be greater than zero when present",
                    );
                }
            }
        }
        finish(issues)
    }
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct AppliedHarnessCommand {
    pub run_state_version: u64,
    pub current_session_revision: u64,
    pub run_status: RunStatus,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct RejectedHarnessCommand {
    pub code: String,
    pub message: String,
    pub actual_state_version: Option<u64>,
}

/// Lease-free HTTP request for appending canonical harness messages.
///
/// Message IDs make an identical retry idempotent. The turn and run state
/// version reject messages produced by stale broker deliveries.
#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct AppendSessionMessages {
    pub expected_state_version: u64,
    pub turn_number: u32,
    pub expected_session_revision: u64,
    /// Requests the tightly fenced post-abort cleanup path. Agent accepts this
    /// only for tool results correlated to calls from the just-aborted turn.
    #[serde(default, skip_serializing_if = "is_false")]
    pub after_cancellation: bool,
    pub messages: Vec<NewRunMessage>,
}

fn is_false(value: &bool) -> bool {
    !*value
}

impl Validate for AppendSessionMessages {
    fn validate(&self) -> Result<(), ValidationError> {
        let mut issues = Vec::new();
        validate_positive(
            &mut issues,
            "expected_state_version",
            self.expected_state_version,
        );
        validate_positive(&mut issues, "turn_number", u64::from(self.turn_number));
        if self.messages.is_empty() {
            issue(&mut issues, "messages", "must contain at least one message");
        }
        let mut ids = HashSet::new();
        for (index, message) in self.messages.iter().enumerate() {
            if !ids.insert(message.session_message_id) {
                issue(
                    &mut issues,
                    format!("messages[{index}].session_message_id"),
                    "must not be duplicated in the batch",
                );
            }
            if let Err(error) = message.validate() {
                append_nested(&mut issues, &format!("messages[{index}]"), error);
            }
            if self.after_cancellation && !matches!(&message.message, Message::ToolResult(_)) {
                issue(
                    &mut issues,
                    format!("messages[{index}].message"),
                    "post-cancellation appends may contain only tool results",
                );
            }
        }
        finish(issues)
    }
}

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
pub struct SessionMessagesAppended {
    pub items: Vec<SessionMessage>,
    pub current_session_revision: u64,
}

fn validate_protocol_version(issues: &mut Vec<llm_contracts::ValidationIssue>, value: u32) {
    if value != HARNESS_PROTOCOL_VERSION {
        issue(
            issues,
            "protocol_version",
            format!("must equal {HARNESS_PROTOCOL_VERSION}"),
        );
    }
}

fn validate_positive(
    issues: &mut Vec<llm_contracts::ValidationIssue>,
    path: &'static str,
    value: u64,
) {
    if value == 0 {
        issue(issues, path, "must be greater than zero");
    }
}

fn validate_uuid(
    issues: &mut Vec<llm_contracts::ValidationIssue>,
    path: &'static str,
    value: Uuid,
) {
    if value.is_nil() {
        issue(issues, path, "must not be nil");
    }
}

fn validate_slug(
    issues: &mut Vec<llm_contracts::ValidationIssue>,
    path: &'static str,
    value: &str,
) {
    let valid_length = (1..=64).contains(&value.len());
    let mut characters = value.chars();
    let valid_start = characters
        .next()
        .is_some_and(|value| value.is_ascii_lowercase());
    let valid_characters = value
        .chars()
        .all(|value| value.is_ascii_lowercase() || value.is_ascii_digit() || value == '-');
    let valid_segments = !value.contains("--") && !value.ends_with('-');
    if !(valid_length && valid_start && valid_characters && valid_segments) {
        issue(
            issues,
            path,
            "must be a lowercase, hyphen-separated harness slug",
        );
    }
}
