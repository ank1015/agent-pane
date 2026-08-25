use std::collections::HashSet;

use chrono::{DateTime, Utc};
use llm_contracts::{Message, Validate, ValidationError};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::{
    HarnessRevision, Run, RunResume, SessionMessage, validation::append_nested, validation::finish,
    validation::issue, validation::validate_id,
};

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ClaimRun {
    pub lease_id: Uuid,
    pub worker_instance_id: String,
    pub supported_harness_revision_ids: Vec<String>,
}

impl Validate for ClaimRun {
    fn validate(&self) -> Result<(), ValidationError> {
        let mut issues = Vec::new();
        validate_id(&mut issues, "worker_instance_id", &self.worker_instance_id);
        if self.supported_harness_revision_ids.is_empty() {
            issue(
                &mut issues,
                "supported_harness_revision_ids",
                "must contain at least one revision",
            );
        }
        let mut seen = HashSet::new();
        for (index, revision_id) in self.supported_harness_revision_ids.iter().enumerate() {
            validate_id(
                &mut issues,
                &format!("supported_harness_revision_ids[{index}]"),
                revision_id,
            );
            if !seen.insert(revision_id) {
                issue(
                    &mut issues,
                    format!("supported_harness_revision_ids[{index}]"),
                    "must not contain duplicates",
                );
            }
        }
        finish(issues)
    }
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct RunLease {
    pub lease_id: Uuid,
    pub run_id: Uuid,
    pub lease_version: u64,
    pub worker_instance_id: String,
    pub acquired_at: DateTime<Utc>,
    pub expires_at: DateTime<Utc>,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
pub struct ClaimedRun {
    pub run: Run,
    pub lease: RunLease,
    pub harness_revision: HarnessRevision,
    pub current_session_revision: u64,
    pub resume: Option<RunResume>,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct HeartbeatRun {
    pub lease_version: u64,
}

impl Validate for HeartbeatRun {
    fn validate(&self) -> Result<(), ValidationError> {
        if self.lease_version == 0 {
            Err(ValidationError::single(
                "lease_version",
                "must be greater than zero",
            ))
        } else {
            Ok(())
        }
    }
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct RunHeartbeat {
    pub lease_id: Uuid,
    pub lease_version: u64,
    pub state_version: u64,
    pub expires_at: DateTime<Utc>,
    pub directives: Vec<crate::WorkerDirective>,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct NewRunMessage {
    pub session_message_id: Uuid,
    pub message: Message,
}

impl Validate for NewRunMessage {
    fn validate(&self) -> Result<(), ValidationError> {
        self.message.validate().map_err(|error| {
            let mut issues = Vec::new();
            append_nested(&mut issues, "message", error);
            ValidationError { issues }
        })
    }
}

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct AppendRunMessages {
    pub lease_version: u64,
    pub expected_session_revision: u64,
    pub messages: Vec<NewRunMessage>,
}

impl Validate for AppendRunMessages {
    fn validate(&self) -> Result<(), ValidationError> {
        let mut issues = Vec::new();
        if self.lease_version == 0 {
            issue(&mut issues, "lease_version", "must be greater than zero");
        }
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
        }
        finish(issues)
    }
}

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
pub struct RunMessagesAppended {
    pub items: Vec<SessionMessage>,
    pub current_session_revision: u64,
}
